;; doge-code.el --- Integration with Doge-Code agent -*- lexical-binding: t; -*-

;; Author: Doge-Code Integration
;; Version: 0.3.0
;; Package-Requires: ((emacs "27.1") (json "1.0") (async "1.9"))

;;; Commentary:
;; Emacs package for integrating with Doge-Code CLI agent.
;; Supports region/buffer analysis, refactoring, explanations.
;; Uses JSON output for structured responses; displays in popup or buffer.
;; Keybindings: Customize via doge-code-mode-map.

;;; Code:

(require 'json)
(require 'async)
(require 'popup)  ; For popup displays
(require 'subr-x)

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

(defun doge-code-cancel ()
  "Cancel the current Doge-Code process."
  (interactive)
  (when (process-live-p doge-code--current-process)
    (kill-process doge-code--current-process)
    (setq doge-code--current-process nil)
    (doge-code--show-progress "Cancelled")))

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
             (funcall callback output))))))

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
         (args (append (list "exec" payload)
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
                       (when (and doge-code-use-popup (not (string-empty-p response)))
                         (popup-tip response :margin t)))
                   (funcall handler nil (or (assoc-default 'error result) "Failed to execute") 0)))
             (funcall handler t output 0))
         (error
          (funcall handler nil (format "JSON parse error: %s\nResponse: %s" err output) 0)))))))

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
  (let ((args (append (list "rewrite" "--prompt" prompt "--code-file" temp-file "--json")
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

;; Enable mode in programming modes
(dolist (mode '(prog-mode c-mode c++-mode python-mode rust-mode js-mode typescript-mode))
  (add-hook (intern (format "%s-hook" mode)) 'doge-code-mode))

(provide 'doge-code)

;;; doge-code.el ends here
