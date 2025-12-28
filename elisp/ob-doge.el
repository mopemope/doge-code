;;; ob-doge.el --- Org-babel functions for Doge-Code evaluation -*- lexical-binding: t; -*-

;; Author: Doge-Code Integration
;; Version: 0.1.0
;; Package-Requires: ((emacs "27.1") (doge-code "0.3.0"))

;;; Commentary:
;; Enable Doge-Code support in Org-mode babel blocks.
;;
;; Usage:
;;
;; #+BEGIN_SRC doge :context "src/main.rs"
;; Analyze this file and suggest refactoring.
;; #+END_SRC
;;
;; #+RESULTS:
;; ... output from doge-code ...

;;; Code:

(require 'ob)
(require 'doge-code)

(defvar org-babel-default-header-args:doge
  '((:results . "output")
    (:exports . "both")
    (:context . nil))
  "Default arguments for doge-code source blocks.")

(defun org-babel-execute:doge (body params)
  "Execute a block of Doge-Code instructions.
BODY is the content of the block (the prompt).
PARAMS is a property list of header arguments.
Supported headers:
:context - File path to provide as context (passed via `doge-code` tools conceptually,
           or just analyzed as the active buffer/file)."
  (let* ((context (cdr (assq :context params)))
         (json-output (cdr (assq :json params))) ;; Optional: if we want raw json
         (instruction (org-babel-trim body))
         (default-directory (or (and context (file-name-directory (expand-file-name context)))
                                default-directory))
         ;; We use a synchronous call for Org-babel usually, or we need to handle async.
         ;; org-babel-execute is typically synchronous. We need `doge-code` to have a sync mode?
         ;; `dgc exec` is a subprocess, so we can run it synchronously with `shell-command-to-string` or `call-process`.
         ;; Reusing `doge-code--exec` logic but synchronously.
         (args (append (list "exec" instruction)
                       (when json-output '("--json")))))
    
    (message "Doge-Code: Executing %s..." (substring instruction 0 (min 20 (length instruction))))
    
    ;; We manually run the process synchronously for Org results
    (with-temp-buffer
      (let ((exit-code
             (apply #'call-process
                    doge-code-executable
                    nil
                    t
                    nil
                    args)))
        (if (eq exit-code 0)
            ;; Success
            (let ((output (buffer-string)))
               ;; Determine if we should format it (e.g. if json)
               ;; For now, raw string output is best for Org.
               output)
          ;; Error
          (format "Error (exit code %d): %s" exit-code (buffer-string)))))))

(provide 'ob-doge)

;;; ob-doge.el ends here
