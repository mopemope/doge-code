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

(defun doge-code--exec (instruction &optional region json-output callback)
  "Execute Doge-Code with INSTRUCTION on REGION.
If JSON-OUTPUT, use --json flag. CALLBACK for async handling."
  (doge-code--show-progress "Processing...")
  
  ;; Cancel any existing process
  (when (process-live-p doge-code--current-process)
    (kill-process doge-code--current-process))
  
  (let* ((code (if region
                   (buffer-substring-no-properties (region-beginning) (region-end))
                 (buffer-string)))
         (json-flag (if json-output " --json" ""))
         (cmd (format "%s --exec '%s %s'%s" doge-code-executable instruction code json-flag))
         (process-environment (cons "DOGE_CODE_NON_INTERACTIVE=1" process-environment)))
    (setq doge-code--current-process
          (async-start
           `(lambda ()
              (let ((process-environment ',process-environment))
                (with-temp-buffer
                  (let ((process (start-process "doge-code" (current-buffer) "sh" "-c" ,cmd)))
                    (unless process
                      (error "Failed to start Doge-Code process"))
                    ;; Set process timeout
                    (with-timeout (,doge-code-timeout
                                   (kill-process process)
                                   (error "Doge-Code process timed out after %s seconds" ,doge-code-timeout))
                      (while (process-live-p process)
                        (accept-process-output process 0.1)))
                    (buffer-string)))))
           (lambda (output)
             (setq doge-code--current-process nil)
             (condition-case err
                 (if json-output
                     (let ((result (json-read-from-string output)))
                       (if (and (assoc 'success result) (assoc-default 'success result))
                           (progn
                             (funcall callback t (or (assoc-default 'response result) "") 
                                      (or (assoc-default 'tokens_used result) 0))
                             (when doge-code-use-popup
                               (popup-tip (or (assoc-default 'response result) "") :margin t)))
                         (funcall callback nil (or (assoc-default 'error result) "Failed to execute") 0)))
                   (funcall callback t output 0))
               (error (funcall callback nil (format "JSON parse error: %s\nResponse: %s" err output) 0))))))))

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
(defun doge-code-refactor-region (start end)
  "Refactor selected region with Doge-Code."
  (interactive "r")
  (deactivate-mark)
  (doge-code--exec "Refactor this code following best practices" (cons start end) t #'doge-code--handle-response))

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