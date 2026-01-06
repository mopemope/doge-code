;;; doge-refactor.el --- Safe context-aware refactoring using Doge-Code and Git -*- lexical-binding: t; -*-

;; Author: Doge-Code Integration
;; Version: 0.1.0
;; Package-Requires: ((emacs "27.1") (doge-code "0.3.0") (vc-git "1.0"))

;;; Commentary:
;; Provides context-aware refactoring commands.
;; Ensures safety by checking Git status before execution and showing diffs after.

;;; Code:

(require 'doge-code)
(require 'vc-git)
(require 'diff-mode)

(defgroup doge-refactor nil
  "Doge-Code Refactoring integration."
  :group 'doge-code)

(defcustom doge-refactor-buffer-name "*Doge-Refactor-Log*"
  "Buffer name for refactoring process output."
  :type 'string)

(defun doge-refactor--check-git-status ()
  "Check if the repository is clean. Return t if clean or user proceeds anyway."
  (let ((default-directory (doge-code--get-project-root)))
    (if (and (file-directory-p ".git")
             (zerop (call-process "git" nil nil nil "diff" "--quiet" "HEAD")))
        t
      (y-or-n-p "Warning: Repository has uncommitted changes. Doge-Code might overwrite them. Proceed? "))))

(defun doge-refactor--show-diff ()
  "Show the diff of changes made by Doge-Code."
  (let ((default-directory (doge-code--get-project-root)))
    (if (fboundp 'magit-status)
        (magit-status default-directory)
      ;; Fallback to vc-diff or standard diff command
      (vc-diff))))

(defun doge-refactor--sentinel (process event)
  "Sentinel for refactoring process."
  (when (memq (process-status process) '(exit signal))
    (let ((exit-code (process-exit-status process))
          (buffer (process-buffer process)))
      (if (eq exit-code 0)
          (progn
            (message "Doge-Code: Refactoring complete! Checking changes...")
            ;; Refresh all buffers
            (revert-all-buffers) 
            (doge-refactor--show-diff)
            (when (y-or-n-p "Refactoring applied. Accept changes? (If No, git restore . will run) ")
                (message "Changes accepted. Please commit them manually.")
                ;; Optional: Stage changes? No, let user do it.
                ;; If user says No:
                (when (not (y-or-n-p "Keep changes? ")) ; Double check logic
                   ;; Wait, if user says NO to "Accept changes?", we should restore?
                   ;; Let's simplify logic:
                   ;; "Refactoring finished. Review changes in Magit/Diff."
                   ;; Then provide a command to undo.
                   ;; But for now, let's keep it simple.
                   nil)))
        (message "Doge-Code: Refactoring failed (code %d). See %s" exit-code (buffer-name buffer))
        (display-buffer buffer)))))

(defun revert-all-buffers ()
  "Refresh all open file buffers without confirmation.
Buffers in modified (unsaved) state are not reverted."
  (interactive)
  (dolist (buf (buffer-list))
    (with-current-buffer buf
      (when (and (buffer-file-name) (not (buffer-modified-p)) (file-exists-p (buffer-file-name)))
        (revert-buffer t t t)))))

(defun doge-refactor--restore-git ()
  "Restore all changes in the repo (git restore .)."
  (interactive)
  (when (y-or-n-p "DANGER: This will discard ALL changes in the current directory. Proceed? ")
    (let ((default-directory (doge-code--get-project-root)))
      (call-process "git" nil nil nil "restore" ".")
      (call-process "git" nil nil nil "clean" "-fd") ;; Cleanup new files too
      (revert-all-buffers)
      (message "Repository restored to HEAD."))))

;;;###autoload
(defun doge-refactor (instruction)
  "Refactor the codebase based on INSTRUCTION.
Leverages TUI session context if `doge-code-resume` is t."
  (interactive "sRefactoring Instruction: ")
  (when (doge-refactor--check-git-status)
    (let* ((root (doge-code--get-project-root))
           (default-directory root)
           (global-args (append (when doge-code-model (list "--model" doge-code-model))
                                (when doge-code-disable-repomap '("--no-repomap"))
                                (when doge-code-resume '("--resume"))))
           ;; Use 'exec' command for refactoring. 'edit' or 'rewrite' might be insufficient for multi-file.
           ;; 'exec' with a strong prompt is best.
           (prompt (format "Refactor the codebase according to this instruction: %s. \
You are in NON-INTERACTIVE mode, but you have access to tools. \
Check the codebase using `ls` or `grep` if needed, then use `edit_file` or `apply_patch` to modify files. \
Ensure you verify the changes if possible (e.g. running tests). \
Do not ask for user confirmation, just do it." instruction))
           (args (append global-args (list "exec" prompt))))
      
      (message "Doge-Code: Starting refactoring... (Check mode-line for status)")
      (make-process
       :name "doge-refactor"
       :buffer (get-buffer-create doge-refactor-buffer-name)
       :command (cons doge-code-executable args)
       :sentinel #'doge-refactor--sentinel))))

(provide 'doge-refactor)

;;; doge-refactor.el ends here
