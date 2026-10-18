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

(defcustom doge-code-resume nil
  "Resume the latest session context (passed as --resume)."
  :type 'boolean)

(defcustom doge-code-enable-auto-mode t
  "Automatically enable `doge-code-mode` in programming modes."
  :type 'boolean)

(defcustom doge-code-enable-refactor t
  "Enable Refactoring tools if `doge-refactor.el` is available."
  :type 'boolean)

(defvar doge-code-mode-map (make-sparse-keymap)
  "Keymap for doge-code-mode.")

(defvar doge-code--current-process nil
  "Current running Doge-Code process.")

(define-minor-mode doge-code-mode
  "Minor mode for Doge-Code integration."
  :lighter " Doge"
  :keymap doge-code-mode-map
  (doge-code-mode-setup))

(defun doge-code-mode-setup ()
  "Setup keybindings for doge-code-mode."
  (define-key doge-code-mode-map (kbd "C-c d a") 'doge-code-analyze-region)
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


;;;###autoload
(defun doge-code-analyze-region (start end)
  "Analyze selected region with Doge-Code (JSON output)."
  (interactive "r")
  (deactivate-mark)
  (doge-code--exec "Analyze this code and suggest improvements" (cons start end) t #'doge-code--handle-response))

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

;; Remove manual initialization as it's handled by doge-code-setup

(provide 'doge-code)

;;; doge-code.el ends here
