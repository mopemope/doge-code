;;; doge-hud.el --- Semantic Heads-Up Display for Doge-Code -*- lexical-binding: t; -*-

;; Author: Doge-Code Integration
;; Version: 0.1.0
;; Package-Requires: ((emacs "27.1") (doge-mcp "0.2.0"))

;;; Commentary:
;; Displays semantic information about the symbol at point using Doge-Code MCP.
;; Information is shown as an overlay at the end of the line (ghost text) when idle.

;;; Code:

(require 'doge-mcp)
(require 'thingatpt)

(defgroup doge-hud nil
  "Doge-Code Semantic HUD."
  :group 'doge-code)

(defcustom doge-hud-idle-delay 0.5
  "Idle delay in seconds before showing HUD."
  :type 'float
  :group 'doge-hud)

(defvar doge-hud--timer nil)
(defvar doge-hud--overlay nil)
(defvar doge-hud--error-count 0 "Consecutive error count for backoff.")
(defconst doge-hud--max-errors 3 "Max consecutive errors before suppressing messages.")

(defun doge-hud--clear-overlay ()
  "Clear the current HUD overlay."
  (when doge-hud--overlay
    (delete-overlay doge-hud--overlay)
    (setq doge-hud--overlay nil)))

(defun doge-hud--show-overlay (summary)
  "Show SUMMARY as an overlay at the end of the current line."
  (doge-hud--clear-overlay)
  (save-excursion
    (end-of-line)
    (let ((ov (make-overlay (point) (point))))
      (overlay-put ov 'after-string (concat "  " (propertize (format ";; %s" summary) 'face 'font-lock-comment-face)))
      (overlay-put ov 'evaporate t)
      (setq doge-hud--overlay ov))))

(defun doge-hud--fetch-and-show ()
  "Fetch info for symbol at point and show it."
  (when (and doge-hud-mode
             (not (minibufferp))
             (not doge-hud--overlay)) ;; Don't fetch if already showing? Or maybe re-fetch.
    (let ((symbol (thing-at-point 'symbol t)))
      (when symbol
        ;; Call MCP search
        ;; Note: This calls standard search_repomap. We might want a more specific tool later.
        ;; For now, we search and take the first likely match.
        (doge-mcp--call-tool "search_repomap" `(("keyword_search" . ,symbol))
                             (lambda (result)
                               ;; Helper to parse result. Result is a list of file matches.
                               (let ((summary nil))
                                 (when result
                                   ;; Look for a symbol match in the result that matches our symbol name
                                   (catch 'found
                                     (dolist (item result)
                                       (let ((symbols (assoc-default 'symbols item)))
                                         (dolist (sym symbols)
                                           (when (string-equal (assoc-default 'name sym) symbol)
                                             ;; Found exact match, use code snippet or kind
                                             (let ((kind (assoc-default 'kind sym))
                                                   (snippet (assoc-default 'code_snippet sym)))
                                               ;; Construct a short summary.
                                               ;; If snippet is available and short, use it.
                                               ;; Otherwise use Kind + Name.
                                               (setq summary (format "%s (%s)" symbol kind))
                                               (throw 'found t))))))))
                                               (setq summary (format "%s (%s)" symbol kind))
                                               (throw 'found t))))))))
                                 (setq doge-hud--error-count 0) ;; Reset error count on success
                                 (when summary
                                   (doge-hud--show-overlay summary)))
                             ;; Error callback (custom handling for HUD)
                             (lambda (&rest _)
                               (setq doge-hud--error-count (1+ doge-hud--error-count))
                               (when (< doge-hud--error-count doge-hud--max-errors)
                                 (message "Doge-HUD: Failed to fetch info (suppressing after %d errors)" doge-hud--max-errors)))))))))

(defun doge-hud--timer-function ()
  "Run by idle timer."
  (doge-hud--fetch-and-show))

(defun doge-hud--post-command ()
  "Clear overlay on any command."
  (doge-hud--clear-overlay))

(defun doge-hud--enable ()
  "Enable HUD timer."
  (unless doge-hud--timer
    (setq doge-hud--timer (run-with-idle-timer doge-hud-idle-delay t #'doge-hud--timer-function))))

(defun doge-hud--disable ()
  "Disable HUD timer if no buffers enable the mode."
  (let ((any-active nil))
    (dolist (buf (buffer-list))
      (with-current-buffer buf
        (when doge-hud-mode
          (setq any-active t))))
    (unless any-active
      (when doge-hud--timer
        (cancel-timer doge-hud--timer)
        (setq doge-hud--timer nil)))))

;;;###autoload
(define-minor-mode doge-hud-mode
  "Toggle Doge-Code Semantic HUD."
  :lighter " D-HUD"
  :global nil
  (if doge-hud-mode
      (progn
        (doge-hud--enable)
        (add-hook 'post-command-hook #'doge-hud--post-command nil t))
    (progn
      (remove-hook 'post-command-hook #'doge-hud--post-command t)
      (doge-hud--clear-overlay)
      (doge-hud--disable))))

(provide 'doge-hud)

;;; doge-hud.el ends here
