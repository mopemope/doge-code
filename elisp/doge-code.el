;; doge-code.el --- Integration with Doge-Code agent -*- lexical-binding: t; -*-

;; Author: Doge-Code Integration
;; Version: 0.3.0
;; Package-Requires: ((emacs "27.1") (json "1.0") (async "1.9"))

;;; Commentary:
;; Emacs package for integrating with Doge-Code CLI agent.
;; Supports region/buffer analysis, refactoring, explanations.
;; This file serves as the main entry point for all Doge-Code extensions.
;; Use `M-x doge-code-setup` to initialize all features.

;;; Code:

(require 'json)
(require 'async)
(require 'subr-x)

;; Optional dependencies for extensions
(defvar doge-code--missing-packages nil
  "List of missing optional packages.")

(defun doge-code--check-package (package feature)
  "Check if PACKAGE is available. If not, add FEATURE to missing list."
  (if (or (featurep package)
          (ignore-errors (require package nil t)))
      t
    (add-to-list 'doge-code--missing-packages package t)
    nil))

(defgroup doge-code nil
  "Doge-Code integration."
  :group 'tools)

(defcustom doge-code-executable "dgc"  ; or full path to binary
  "Path to doge-code binary."
  :type 'string)

(defcustom doge-code-use-popup t
  "Display responses in popup if non-nil, else in *doge-output* buffer."
  :type 'boolean)

(defcustom doge-code-show-progress t
  "Show progress message during Doge-Code execution."
  :type 'boolean)

(defcustom doge-code-timeout 300
  "Timeout for Doge-Code execution in seconds."
  :type 'integer)

(defcustom doge-code-model nil
  "Model name to use (e.g. gpt-4o). If nil, use default from config/env."
  :type '(choice (const :tag "Default" nil) string))

(defcustom doge-code-disable-repomap nil
  "Disable Repomap generation for faster startup (passed as --no-repomap)."
  :type 'boolean)

(defcustom doge-code-fix-retry 3
  "Number of retries for the fix command."
  :type 'integer)


(defcustom doge-code-resume nil
  "Resume the latest session context (passed as --resume)."
  :type 'boolean)

(defcustom doge-code-enable-auto-mode t
  "Automatically enable `doge-code-mode` in programming modes."
  :type 'boolean)

(defcustom doge-code-enable-mcp t
  "Enable MCP integration if `doge-mcp.el` and dependencies are available."
  :type 'boolean)

(defcustom doge-code-enable-hud nil
  "Enable Semantic HUD if `doge-hud.el` is available."
  :type 'boolean)

(defcustom doge-code-enable-flymake t
  "Enable Flymake integration if `doge-flymake.el` is available."
  :type 'boolean)

(defcustom doge-code-enable-compile t
  "Enable Compilation auto-fix if `doge-compile.el` is available."
  :type 'boolean)

(defcustom doge-code-enable-refactor t
  "Enable Refactoring tools if `doge-refactor.el` is available."
  :type 'boolean)

(defvar doge-code-mode-map (make-sparse-keymap)
  "Keymap for doge-code-mode.")

(defvar doge-code--current-process nil
  "Current running Doge-Code process.")

(defvar doge-code--prompt-history nil
  "Minibuffer history for Doge-Code rewrite prompts.")

(define-minor-mode doge-code-mode
  "Minor mode for Doge-Code integration."
  :lighter " Doge"
  :keymap doge-code-mode-map
  (doge-code-mode-setup))

(defun doge-code-mode-setup ()
  "Setup keybindings for doge-code-mode."
  (define-key doge-code-mode-map (kbd "C-c d a") 'doge-code-analyze-region)
  (define-key doge-code-mode-map (kbd "C-c d r") 'doge-code-refactor-region)
  (define-key doge-code-mode-map (kbd "C-c d e") 'doge-code-explain-region)
  (define-key doge-code-mode-map (kbd "C-c d b") 'doge-code-analyze-buffer)
  ;; Cancel command
  (define-key doge-code-mode-map (kbd "C-c d c") 'doge-code-cancel))

(defun doge-code--show-progress (message)
  "Show progress MESSAGE if enabled."
  (when doge-code-show-progress
    (message "Doge-Code: %s" message)))

(defun doge-code--handle-response (success response tokens)
  "Handle response from Doge-Code."
  (if success
      (progn
        (doge-code--show-progress "Completed")
        (if (and doge-code-use-popup (featurep 'popup))
            (unless (active-minibuffer-window)
              (message "Doge-Code: %s (Tokens: %d)" response tokens))
          (with-current-buffer (get-buffer-create "*doge-output*")
            (erase-buffer)
            (insert response)
            (display-buffer (current-buffer)))))
    (progn
      (doge-code--show-progress "Error occurred")
      (message "Doge-Code Error: %s" response))))

(defun doge-code--get-project-root ()
  "Get the project root directory."
  (or (and (fboundp 'project-current)
           (project-root (project-current)))
      default-directory))

;;;###autoload
(defun doge-code-setup ()
  "Initialize Doge-Code and its extensions."
  (interactive)
  (setq doge-code--missing-packages nil)
  
  ;; Initialize keybindings
  (doge-code-mode-setup)

  ;; Setup auto mode
  (when doge-code-enable-auto-mode
    (dolist (mode '(prog-mode c-mode c++-mode python-mode rust-mode js-mode typescript-mode))
      (add-hook (intern (format "%s-hook" mode)) 'doge-code-mode)))

  ;; Load extensions
  (let ((load-path (cons (file-name-directory (or load-file-name (buffer-file-name))) load-path)))
    
    ;; Try loading popup for better UI
    (doge-code--check-package 'popup 'doge-code)

    ;; MCP
    (when (and doge-code-enable-mcp
               (doge-code--check-package 'request 'doge-mcp)
               (doge-code--check-package 'deferred 'doge-mcp))
      (when (require 'doge-mcp nil t)
        (message "Doge-Code: MCP integration enabled.")))

    ;; HUD (requires MCP)
    (when (and doge-code-enable-hud (featurep 'doge-mcp))
      (when (require 'doge-hud nil t)
        (doge-hud-mode 1)
        (message "Doge-Code: Semantic HUD enabled.")))

    ;; Flymake
    (when doge-code-enable-flymake
      (when (require 'doge-flymake nil t)
        (define-key doge-code-mode-map (kbd "C-c d f") 'doge-flymake-fix-at-point)
        (message "Doge-Code: Flymake integration enabled.")))

    ;; Compile
    (when doge-code-enable-compile
      (when (require 'doge-compile nil t)
        (doge-compile-mode 1)
        (message "Doge-Code: Compilation auto-fix enabled.")))

    ;; Refactor
    (when doge-code-enable-refactor
      (when (require 'doge-refactor nil t)
        (define-key doge-code-mode-map (kbd "C-c d R") 'doge-refactor)
        (message "Doge-Code: Refactoring tools enabled.")))

    ;; Org-babel
    (when (require 'ob-doge nil t)
      (with-eval-after-load 'org
        (org-babel-do-load-languages
         'org-babel-load-languages
         (append org-babel-load-languages '((doge . t))))))

    (when doge-code--missing-packages
      (message "Doge-Code Notice: Some features disabled due to missing packages: %s"
               (mapconcat #'symbol-name doge-code--missing-packages ", ")))))

(defun doge-code--async-run (args callback)
  "Execute Doge-Code binary with ARGS asynchronously and invoke CALLBACK with output.
ARGS is a list of command-line arguments passed to `doge-code-executable`.
CALLBACK is called with the raw stdout string when the process completes."
  (doge-code--show-progress "Processing...")

  (when (process-live-p doge-code--current-process)
    (kill-process doge-code--current-process))

  (let* ((process-environment (cons "DOGE_CODE_NON_INTERACTIVE=1" process-environment))
         (binary doge-code-executable)
         (args-list args)
         (timeout doge-code-timeout))
    (unless (executable-find binary)
      (error "Doge-Code executable not found: %s" binary))
    (setq doge-code--current-process
          (async-start
           `(lambda ()
              (let ((process-environment ',process-environment))
                (with-temp-buffer
                  (let* ((cmd-args (cons ,binary ',args-list))
                         (process (apply #'start-process "doge-code" (current-buffer) cmd-args)))
                    (unless process
                      (error "Failed to start Doge-Code process"))
                    (with-timeout (,timeout
                                   (kill-process process)
                                   (error "Doge-Code process timed out after %s seconds" ,timeout))
                      (while (process-live-p process)
                        (accept-process-output process 0.1)))
                    (buffer-string)))))
           (lambda (output)
             (setq doge-code--current-process nil)
             (funcall callback (decode-coding-string output 'utf-8))))))

(defun doge-code--exec (instruction &optional region json-output callback)
  "Execute Doge-Code with INSTRUCTION on REGION.
If JSON-OUTPUT, add --json flag. CALLBACK defaults to `doge-code--handle-response'."
  (let* ((range (and (consp region) region))
         (code (if range
                   (buffer-substring-no-properties (car range) (cdr range))
                 (buffer-string)))
         (payload (if (and code (> (length code) 0))
                      (format "%s\n\n%s" instruction code)
                    instruction))
         (global-args (append (when doge-code-model (list "--model" doge-code-model))
                              (when doge-code-disable-repomap '("--no-repomap"))
                              (when doge-code-resume '("--resume"))))
         (args (append global-args
                       (list "exec" payload)
                       (when json-output '("--json"))))
         (handler (or callback #'doge-code--handle-response)))
    (doge-code--async-run
     args
     (lambda (output)
       (condition-case err
           (if json-output
               (let ((result (json-read-from-string output)))
                 (if (and (assoc 'success result) (assoc-default 'success result))
                     (let ((response (or (assoc-default 'response result) ""))
                           (tokens (or (assoc-default 'tokens_used result) 0)))
                       (funcall handler t response tokens)
                       (when (and doge-code-use-popup (featurep 'popup) (not (string-empty-p response)))
                         (popup-tip response :margin t)))
                   (funcall handler nil (or (assoc-default 'error result) "Failed to execute") 0)))
             (funcall handler t output 0))
         (error
          (funcall handler nil (format "JSON parse error: %s\nRaw Output: %s" err output) 0)))))))

(defun doge-code--handle-response (success response tokens)
  "Handle response from Doge-Code."
  (if success
      (progn
        (doge-code--show-progress "Completed")
        (if doge-code-use-popup
            (unless (active-minibuffer-window)
              (message "Doge-Code: %s (Tokens: %d)" response tokens))
          (with-current-buffer (get-buffer-create "*doge-output*")
            (erase-buffer)
            (insert response)
            (display-buffer (current-buffer)))))
    (progn
      (doge-code--show-progress "Error occurred")
      (message "Doge-Code Error: %s" response))))

(defun doge-code--rewrite-snippet (prompt temp-file buffer start-marker end-marker file-path original-snippet)
  "Rewrite snippet by invoking Doge-Code rewrite subcommand.
PROMPT is the user instruction.
TEMP-FILE contains the snippet to rewrite.
BUFFER is the target buffer that should receive the rewrite.
START-MARKER and END-MARKER delimit the region to replace.
FILE-PATH optionally provides context to the CLI.
ORIGINAL-SNIPPET is used to ensure the buffer has not changed before applying the rewrite."
  (let* ((global-args (append (when doge-code-model (list "--model" doge-code-model))
                              (when doge-code-disable-repomap '("--no-repomap"))))
         (args (append global-args
                       (list "rewrite" "--prompt" prompt "--code-file" temp-file "--json")
                       (when file-path (list "--file-path" file-path)))))
    (doge-code--async-run
     args
     (lambda (output)
       (unwind-protect
           (if (not (buffer-live-p buffer))
               (progn
                 (doge-code--show-progress "Error occurred")
                 (message "Doge-Code rewrite aborted: buffer closed"))
             (with-current-buffer buffer
               (condition-case err
                   (let ((result (json-read-from-string output)))
                     (if (and (assoc 'success result) (assoc-default 'success result))
                         (let* ((rewritten-raw (assoc-default 'rewritten_code result))
                                (rewritten (unless (eq rewritten-raw json-null) rewritten-raw))
                                (tokens-raw (assoc-default 'tokens_used result))
                                (tokens (if (or (null tokens-raw) (eq tokens-raw json-null)) 0 tokens-raw))
                                (display-path-raw (assoc-default 'display_path result))
                                (display-path (when (and display-path-raw (not (eq display-path-raw json-null)))
                                                display-path-raw)))
                            (if (not rewritten)
                                (progn
                                  (doge-code--show-progress "Error occurred")
                                  (message "Doge-Code Error: rewrite result missing rewritten_code"))
                              (let ((beg (marker-position start-marker))
                                    (end (marker-position end-marker)))
                                (if (and beg end)
                                    (let ((current-snippet (buffer-substring-no-properties beg end)))
                                      (if (not (string= current-snippet original-snippet))
                                          (progn
                                            (doge-code--show-progress "Error occurred")
                                            (message "Doge-Code rewrite aborted: region changed during rewrite"))
                                        (progn
                                          (save-excursion
                                            (let ((inhibit-read-only t))
                                              (goto-char beg)
                                              (delete-region beg end)
                                              (insert rewritten)))
                                          (doge-code--show-progress "Completed")
                                          (if display-path
                                              (message "Doge-Code rewrite applied to %s (tokens: %d)" display-path tokens)
                                            (message "Doge-Code rewrite applied (tokens: %d)" tokens)))))
                                  (progn
                                    (doge-code--show-progress "Error occurred")
                                    (message "Doge-Code rewrite aborted: region changed"))))))
                      (doge-code--show-progress "Error occurred")
                      (message "Doge-Code Error: %s"
                               (or (assoc-default 'error result) "Rewrite failed"))))
                 (error
                 (doge-code--show-progress "Error occurred")
                  (message "Doge-Code Error: %s\nResponse: %s" err output)))))
         (ignore-errors (delete-file temp-file))
         (set-marker start-marker nil)
         (set-marker end-marker nil))))))

;;;###autoload
(defun doge-code-analyze-region (start end)
  "Analyze selected region with Doge-Code (JSON output)."
  (interactive "r")
  (deactivate-mark)
  (doge-code--exec "Analyze this code and suggest improvements" (cons start end) t #'doge-code--handle-response))

;;;###autoload
(defun doge-code-refactor-region (start end)
  "Rewrite the active region (or entire buffer) with a custom Doge-Code prompt."
  (interactive "r")
  (let* ((has-region (use-region-p))
         (beg (if has-region start (point-min)))
         (end (if has-region end (point-max)))
         (prompt (read-string "Rewrite prompt: " nil 'doge-code--prompt-history)))
    (when (string-empty-p prompt)
      (user-error "Rewrite prompt cannot be empty"))
    (let* ((snippet (buffer-substring-no-properties beg end))
           (temp-file (make-temp-file "doge-code-snippet" nil ".txt"))
           (target-buffer (current-buffer))
           (start-marker (copy-marker beg))
           (end-marker (copy-marker end t))
           (file-path (when buffer-file-name (expand-file-name buffer-file-name))))
      (when (string-empty-p snippet)
        (user-error "Selected region is empty"))
      (with-temp-file temp-file
        (insert snippet))
      (doge-code--rewrite-snippet prompt temp-file target-buffer start-marker end-marker file-path snippet)
      (when has-region
        (deactivate-mark)))))

;;;###autoload
(defun doge-code-explain-region (start end)
  "Explain selected region with Doge-Code (plain output)."
  (interactive "r")
  (deactivate-mark)
  (doge-code--exec "Explain what this code does" (cons start end) nil #'doge-code--handle-response))

;;;###autoload
(defun doge-code-analyze-buffer ()
  "Analyze current buffer with Doge-Code."
  (interactive)
  (doge-code--exec "Analyze the entire file and suggest improvements" nil t #'doge-code--handle-response))

;;;###autoload
(defun doge-code-exec-interactive (instruction)
  "Execute a generic Doge-Code instruction."
  (interactive "sInstruction: ")
  (doge-code--exec instruction nil t #'doge-code--handle-response))

;;;###autoload
(defalias 'doge-code-exec 'doge-code-exec-interactive)

;;;###autoload
(defalias 'doge-code-rewrite-region 'doge-code-refactor-region)

;;;###autoload
(defun doge-code-fix ()
  "Attempt to fix the last failed command (default: compile-command).
Runs the command via 'dgc fix' and displays output in a buffer."
  (interactive)
  (let ((cmd (read-string "Command to fix: " compile-command)))
    (let ((buffer (get-buffer-create "*doge-code-fix*")))
      (with-current-buffer buffer
        (erase-buffer)
        (insert (format "Running: %s fix \"%s\"\n\n" doge-code-executable cmd)))
      (display-buffer buffer)
      (make-process
       :name "doge-code-fix"
       :buffer buffer
       :command (append (list doge-code-executable)
                        (when doge-code-model (list "--model" doge-code-model))
                        (when doge-code-disable-repomap '("--no-repomap"))
                        (list "fix" cmd "--retry" (number-to-string doge-code-fix-retry)))
       :sentinel (lambda (proc event)
                   (when (string= event "finished\n")
                     (with-current-buffer (process-buffer proc)
                       (insert "\n[Done]"))
                     (message "Doge-Code: Fix attempt finished.")))))))

;; Remove manual initialization as it's handled by doge-code-setup

(provide 'doge-code)

;;; doge-code.el ends here
