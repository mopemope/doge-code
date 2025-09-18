;; doge-code-mcp.el --- MCP Client for Doge-Code -*- lexical-binding: t; -*-

;; Author: Doge-Code MCP Extension
;; Version: 0.1.0
;; Package-Requires: ((emacs \"27.1\") (request \"0.3.0\") (json \"1.0\") (deferred \"0.5.1\"))

;;; Commentary:
;; MCP client integration for Doge-Code MCP server.
;; Connects to MCP HTTP server for tool calls (e.g., search_repomap).
;; Usage: M-x doge-mcp-call-tool to invoke tools like search_repomap.

;;; Code:

(require 'request)
(require 'json)
(require 'deferred)

(defgroup doge-mcp nil
  \"Doge-Code MCP client.\"
  :group 'tools)

(defcustom doge-mcp-server-url "http://127.0.0.1:8000"
  \"URL of Doge-Code MCP server.\"
  :type 'string)

(defcustom doge-mcp-default-tools '("search_repomap" "fs_read")
  \"Default tools to query.\"
  :type '(repeat string))

(defun doge-mcp--call-tool (tool-name params callback)
  \"Call MCP tool TOOL-NAME with PARAMS, call CALLBACK with result.\"
  (let ((url (concat doge-mcp-server-url "/mcp/call_tool"))
        (data (json-encode `((name . ,tool-name) (arguments . ,params)))))
    (request url
      :type "POST"
      :data data
      :headers '(("Content-Type" . "application/json"))
      :parser (lambda () (decode-coding-string (buffer-string) 'utf-8))
      :success (cl-function (lambda (&rest _args)
                             (let ((response (json-read-from-string (buffer-string))))
                               (funcall callback (assoc-default 'result response))))
      :error (cl-function (lambda (&rest _args)
                            (message "MCP call failed: %s" (buffer-string)))))))

(defun doge-mcp-list-tools ()
  \"List available MCP tools.\"
  (interactive)
  (doge-mcp--call-tool "list_tools" nil (lambda (tools) (message "Tools: %s" tools))))

;;;###autoload
(defun doge-mcp-search-repomap (keywords)
  \"Search repomap with KEYWORDS.\"
  (interactive (list (read-string "Keywords: ")))
  (let ((params `(("keyword_search" . ,keywords))))
    (doge-mcp--call-tool "search_repomap" params (lambda (result)
                                                   (with-current-buffer "*doge-mcp-output*"
                                                     (erase-buffer)
                                                     (insert (format "Search Results:\n%s" (cdr (assoc 'result (car result)))))))))

;;;###autoload
(defun doge-mcp-fs-read (path)
  \"Read file at PATH via MCP.\"
  (interactive (list (read-file-name "Path: ")))
  (let ((params `(("path" . ,path))))
    (doge-mcp--call-tool "fs_read" params (lambda (result)
                                             (with-current-buffer "*doge-mcp-output*"
                                               (erase-buffer)
                                               (insert (cdr (assoc 'result (car result)))))))))

;; Bind in doge-code-mode
(with-eval-after-load 'doge-code
  (define-key doge-code-mode-map (kbd "C-c d m s") 'doge-mcp-search-repomap)
  (define-key doge-code-mode-map (kbd "C-c d m f") 'doge-mcp-fs-read))

(provide 'doge-mcp)

;;; doge-code-mcp.el ends here