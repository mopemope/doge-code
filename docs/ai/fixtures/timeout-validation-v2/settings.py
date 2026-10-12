"""Normalize the worker timeout value."""
def parse_timeout(value):
    seconds = float(value)
    if seconds <= 0:
        raise ValueError("timeout must be positive")
    return seconds
