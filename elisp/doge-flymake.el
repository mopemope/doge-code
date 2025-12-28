;;; doge-flymake.el --- Self-Healing Flymake for Doge-Code -*- lexical-binding: t; -*-

;; Author: Doge-Code Integration
;; Version: 0.1.0
;; Package-Requires: ((emacs "27.1") (doge-code "0.3.0") (flymake "1.0"))

;;; Commentary:
;; Provides commands to fix Flymake errors using Doge-Code.

;;; Code:

(require 'flymake)
(require 'doge-code)

(defgroup doge-flymake nil
  "Doge-Code Flymake integration."
  :group 'doge-code)

;;;###autoload
(defun doge-flymake-fix-at-point ()
  "Fix the Flymake diagnostic at point using Doge-Code."
  (interactive)
  (let ((diags (flymake-diagnostics-at-point)))
    (unless diags
      (user-error "No Flymake diagnostics at point"))
    ;; Use the first diagnostic
    (let* ((diag (car diags))
           (msg (flymake-diagnostic-text diag))
           (diag-beg (flymake-diagnostic-beg diag))
           (diag-end (flymake-diagnostic-end diag))
           ;; Expand to whole lines to give LLM context
           (beg (save-excursion (goto-char diag-beg) (line-beginning-position)))
           (end (save-excursion (goto-char diag-end) (line-end-position)))
           (region-content (buffer-substring-no-properties beg end))
           (prompt (format "Fix this error: \"%s\" on the following code:\n%s" msg region-content))
           ;; Setup arguments for doge-code--rewrite-snippet
           (temp-file (make-temp-file "doge-fix-snippet" nil ".txt"))
           (target-buffer (current-buffer))
           (start-marker (copy-marker beg))
           (end-marker (copy-marker end t))
           (file-path (when buffer-file-name (expand-file-name buffer-file-name))))
      
      (message "Automatic Fix: %s..." msg)
      
      ;; Write snippet to temp file
      (with-temp-file temp-file
        (insert region-content))

      ;; Call doge code helper
      (doge-code--rewrite-snippet
       prompt
       temp-file
       target-buffer
       start-marker
       end-marker
       file-path
       region-content))))

(provide 'doge-flymake)

;;; doge-flymake.el ends here
