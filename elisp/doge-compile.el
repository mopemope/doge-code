;;; doge-compile.el --- Auto-fix compilation errors using Doge-Code -*- lexical-binding: t; -*-

;; Author: Doge-Code Integration
;; Version: 0.1.0
;; Package-Requires: ((emacs "27.1") (doge-code "0.3.0") (json "1.0"))

;;; Commentary:
;; Automatically attempts to fix compilation errors using `dgc fix`.
;; Hooks into `compilation-finish-functions`.

;;; Code:

(require 'doge-code)
(require 'json)
(require 'compile)

(defgroup doge-compile nil
  "Doge-Code compilation integration."
  :group 'doge-code)

(defcustom doge-compile-auto-fix 'ask
  "Whether to automatically fix compilation errors.
Possible values:
- nil: Disable auto-fix.
- t: Automatically try to fix without confirmation.
- 'ask: Ask the user before fixing."
  :type '(choice (const :tag "Disable" nil)
                 (const :tag "Always Fix" t)
                 (const :tag "Ask User" ask)))

(defun doge-compile--fix-command (command)
  "Run `dgc fix` with COMMAND and return the report."
  (let* ((global-args (append (when doge-code-model (list "--model" doge-code-model))
                              (when doge-code-disable-repomap '("--no-repomap"))
                              (when doge-code-resume '("--resume"))))
         (args (append global-args
                       (list "fix" command
                             "--json"
                             "--retry" (number-to-string doge-code-fix-retry)))))
    (message "Doge-Code: Attempting to fix compilation errors...")
    (with-temp-buffer
      (let ((exit-code (apply #'call-process doge-code-executable nil t nil args)))
        (if (eq exit-code 0)
            (condition-case err
                (json-read-from-string (buffer-string))
              (error
               (message "Doge-Code: Failed to parse fix report: %s" err)
               nil))
          (message "Doge-Code: Fix command failed (exit code %d)" exit-code)
          nil)))))

(defun doge-compile--handle-finish (buffer status)
  "Handle compilation finish. BUFFER is the compilation buffer, STATUS is the string status."
  (when (and doge-compile-auto-fix
             (string-match-p "exited abnormally" status))
    (let ((command compile-command)) ; compile-command is buffer-local in compilation buffer
      (when (if (eq doge-compile-auto-fix 'ask)
                (y-or-n-p (format "Compilation failed. Attempt auto-fix with Doge-Code? "))
              t)
        (let ((report (doge-compile--fix-command command)))
          (if (and report (assoc-default 'success report))
              (progn
                (message "Doge-Code: Fix applied successfully!")
                ;; Reload buffers if they changed?
                ;; Revert all buffers visiting changed files
                (dolist (attempt (append (assoc-default 'attempts report) nil))
                   ;; In a real implementation we might assume `dgc` handles file writes.
                   ;; We should ideally revert buffers that were modified.
                   ;; For now, we revert the current buffer if it matches?
                   ;; Or just tell the user to revert.
                   (message "  %s" (or (assoc-default 'ai_fix_summary attempt) "Fixed.")))
                
                (when (y-or-n-p "Fix applied. Recompile? ")
                  (recompile)))
            (message "Doge-Code: Could not fix compilation errors. See logs.")))))))

;;;###autoload
(define-minor-mode doge-compile-mode
  "Toggle Doge-Code compilation integration."
  :global t
  (if doge-compile-mode
      (add-hook 'compilation-finish-functions #'doge-compile--handle-finish)
    (remove-hook 'compilation-finish-functions #'doge-compile--handle-finish)))

(provide 'doge-compile)

;;; doge-compile.el ends here
