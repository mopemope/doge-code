;; doge-code.el --- Integration with Doge-Code agent -*- lexical-binding: t; -*-

;; Author: Doge-Code Integration
;; Version: 0.2.0
;; Package-Requires: ((emacs \"27.1\") (json \"1.0\") (async \"1.9\"))

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
  \"Doge-Code integration.\"
  :group 'tools)

(defcustom doge-code-executable \"dgc\"  ; or full path to binary
  \"Path to doge-code binary.\"
  :type 'string)

(defcustom doge-code-use-popup t
  \"Display responses in popup if non-nil, else in *doge-output* buffer.\"
  :type 'boolean)

(defvar doge-code-mode-map (make-sparse-keymap)
  \"Keymap for doge-code-mode.\")

(define-minor-mode doge-code-mode
  \"Minor mode for Doge-Code integration.\"
  :lighter \" Doge\"
  :keymap doge-code-mode-map
  (doge-code-mode-setup))

(defun doge-code-mode-setup ()
  \"Setup keybindings for doge-code-mode.\"
  (define-key doge-code-mode-map (kbd \"C-c d a\") 'doge-code-analyze-region)
  (define-key doge-code-mode-map (kbd \"C-c d r\") 'doge-code-refactor-region)
  (define-key doge-code-mode-map (kbd \"C-c d e\") 'doge-code-explain-region)
  (define-key doge-code-mode-map (kbd \"C-c d b\") 'doge-code-analyze-buffer))

(defun doge-code--exec (instruction &optional region json-output callback)
  \"Execute Doge-Code with INSTRUCTION on REGION.
If JSON-OUTPUT, use --json flag. CALLBACK for async handling.\"
  (let* ((code (if region
                   (buffer-substring-no-properties (region-beginning) (region-end))
                 (buffer-string)))
         (json-flag (if json-output \" --json\" \"\"))
         (cmd (format \"%s --exec '%s %s'%s\" doge-code-executable instruction code json-flag)))
    (async-start
     (lambda (process)
       (with-current-buffer (process-buffer process)
         (let ((output (buffer-string)))
           (if json-output
               (condition-case err
                   (let ((result (json-read-from-string output)))
                     (if (eq (process-exit-status process) 0)
                         (progn
                           (funcall callback t (gethash \"response\" result \"\") (gethash \"tokens_used\" result 0))
                           (when doge-code-use-popup
                             (popup-tip (gethash \"response\" result \"\") :margin t)))
                       (funcall callback nil (gethash \"error\" result \"Failed to execute\") 0)))
                 (error (funcall callback nil (format \"JSON parse error: %s\" err) 0)))
             (funcall callback t output 0)))))
     nil)))

(defun doge-code--handle-response (success response tokens)
  \"Handle response from Doge-Code.\"
  (if success
      (message \"Doge-Code: %s (Tokens: %d)\" response tokens)
    (message \"Doge-Code Error: %s\" response)))

;;;###autoload
(defun doge-code-analyze-region (start end)
  \"Analyze selected region with Doge-Code (JSON output).\"
  (interactive \"r\")
  (deactivate-mark)
  (doge-code--exec \"Analyze this code and suggest improvements\" (cons start end) t #'doge-code--handle-response))

;;;###autoload
(defun doge-code-refactor-region (start end)
  \"Refactor selected region with Doge-Code.\"
  (interactive \"r\")
  (deactivate-mark)
  (doge-code--exec \"Refactor this code following best practices\" (cons start end) t #'doge-code--handle-response))

;;;###autoload
(defun doge-code-explain-region (start end)
  \"Explain selected region with Doge-Code (plain output).\"
  (interactive \"r\")
  (deactivate-mark)
  (doge-code--exec \"Explain what this code does\" (cons start end) nil #'doge-code--handle-response))

;;;###autoload
(defun doge-code-analyze-buffer ()
  \"Analyze current buffer with Doge-Code.\"
  (interactive)
  (doge-code--exec \"Analyze the entire file and suggest improvements\" nil t #'doge-code--handle-response))

;; Enable mode in programming modes
(dolist (mode '(prog-mode c-mode c++-mode python-mode rust-mode js-mode typescript-mode))
  (add-hook (intern (format "%s-hook" mode)) 'doge-code-mode))

(provide 'doge-code)

;;; doge-code.el ends here