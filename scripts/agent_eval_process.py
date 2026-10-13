"""Managed child capture shared by offline checks and live evaluation entrypoints.

This mechanism has no model, API budget, trial accounting or evidence policy.
Callers supply the command, environment, deadlines and capture destinations.
"""
import os
import signal
import subprocess
import threading
import time

MAX_STREAM_BYTES = 200 * 1024 * 1024
MAX_FINAL_DRAIN_BYTES = 1024 * 1024


def stream_child(argv, cwd, env, timeout_seconds, grace_seconds,
                 stdout_path, stderr_path):
    """Run a child with bounded, concurrently drained output files.

    Give the leader time to flush on cancellation, then kill its original
    process group even if the leader already exited. A completed trial must
    not leave same-group descendants writing into its workspace. Detached
    sessions are outside this group boundary; stop readers before returning
    so they cannot change captured artifacts after collection finishes.
    """
    start = time.monotonic()
    timed_out = False
    truncated = [False, False]
    readers = []
    reader_errors = []
    stop_readers = threading.Event()

    def drain(pipe, path, index):
        try:
            with pipe, open(path, "wb") as handle:
                os.set_blocking(pipe.fileno(), False)
                remaining = MAX_STREAM_BYTES
                final_remaining = None
                while True:
                    if stop_readers.is_set() and final_remaining is None:
                        final_remaining = MAX_FINAL_DRAIN_BYTES
                    try:
                        read_size = 64 * 1024 if final_remaining is None else min(64 * 1024, final_remaining)
                        chunk = os.read(pipe.fileno(), read_size)
                    except BlockingIOError:
                        if stop_readers.is_set():
                            reader_errors.append("output_capture_incomplete: output pipe still open at final drain")
                            break
                        stop_readers.wait(0.01)
                        continue
                    if not chunk:
                        break
                    kept = chunk[:remaining]
                    handle.write(kept)
                    remaining -= len(kept)
                    if len(kept) != len(chunk):
                        truncated[index] = True
                    if final_remaining is not None:
                        # Count consumed bytes even after the capture sink is full.
                        final_remaining -= len(chunk)
                        if final_remaining == 0:
                            reader_errors.append("output_capture_incomplete: final drain byte limit reached before EOF")
                            break
        except OSError as error:
            reader_errors.append(f"output_capture_failed: {error}")

    def kill_group():
        try:
            if os.name == "posix":
                # start_new_session makes pid the group id. Do not query
                # getpgid after wait(): the leader may already be reaped.
                os.killpg(child.pid, signal.SIGKILL)
            elif child.poll() is None:
                child.kill()
        except ProcessLookupError:
            pass

    def stop_child():
        try:
            child.terminate()
        except ProcessLookupError:
            pass
        try:
            child.wait(timeout=grace_seconds)
        except subprocess.TimeoutExpired:
            pass
        finally:
            kill_group()
        return child.wait()

    # Check the sinks before starting a process that might mutate a workspace.
    try:
        stdout_path.write_bytes(b"")
        stderr_path.write_bytes(b"")
        child = subprocess.Popen(
            argv, cwd=str(cwd), env=env, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, start_new_session=True,
        )
    except OSError as error:
        try:
            stderr_path.write_bytes(
                f"harness: failed to spawn child: {error}\n".encode("utf-8")
            )
        except OSError:
            pass
        return 127, False, 0.0, f"spawn_failed: {error}", False

    try:
        for index, (pipe, path) in enumerate((
            (child.stdout, stdout_path), (child.stderr, stderr_path),
        )):
            reader = threading.Thread(target=drain, args=(pipe, path, index))
            readers.append(reader)
            reader.start()
        try:
            exit_code = child.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            timed_out = True
            exit_code = stop_child()
        except KeyboardInterrupt:
            stop_child()
            raise
    finally:
        kill_group()
        child.wait()
        for reader in readers:
            reader.join(timeout=grace_seconds)
        if any(reader.is_alive() for reader in readers):
            # A live reader can merely be unscheduled. Let it distinguish real
            # EOF from an open pipe using a bounded, nonblocking final drain.
            stop_readers.set()
            for reader in readers:
                reader.join()
    error = "; ".join(reader_errors) if reader_errors else None
    return exit_code, timed_out, time.monotonic() - start, error, any(truncated)
