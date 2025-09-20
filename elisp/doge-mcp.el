;; doge-code-mcp.el --- MCP Client for Doge-Code -*- lexical-binding: t; -*-

;; Author: Doge-Code MCP Extension
;; Version: 0.2.0
;; Package-Requires: ((emacs "27.1") (request "0.3.0") (json "1.0") (deferred "0.5.1"))

;;; Commentary:
;; MCP client integration for Doge-Code MCP server.
;; Connects to MCP HTTP server for tool calls (e.g., search_repomap).
;; Usage: M-x doge-mcp-call-tool to invoke tools like search_repomap.

;;; Code:

(require 'request)
(require 'json)
(require 'deferred)

(defgroup doge-mcp nil
  "Doge-Code MCP client."
  :group 'tools)

(defcustom doge-mcp-server-url "http://127.0.0.1:8000"
  "URL of Doge-Code MCP server."
  :type 'string)

(defcustom doge-mcp-default-tools '("search_repomap" "fs_read")
  "Default tools to query."
  :type '(repeat string))

(defcustom doge-mcp-timeout 30
  "Timeout for MCP requests in seconds."
  :type 'integer)

(defcustom doge-mcp-show-progress t
  "Show progress messages for MCP operations."
  :type 'boolean)

(defun doge-mcp--show-progress (message)
  "Show progress MESSAGE if enabled."
  (when doge-mcp-show-progress
    (message "Doge-MCP: %s" message)))

(defun doge-mcp--call-tool (tool-name params callback)
  "Call MCP tool TOOL-NAME with PARAMS, call CALLBACK with result."
  (doge-mcp--show-progress (format "Calling %s..." tool-name))
  (let ((url (concat doge-mcp-server-url "/mcp/call_tool"))
        (data (json-encode `((name . ,tool-name) (arguments . ,params)))))
    (request url
      :type "POST"
      :data data
      :headers '(("Content-Type" . "application/json"))
      :timeout doge-mcp-timeout
      :parser (lambda () (decode-coding-string (buffer-string) 'utf-8))
      :success (cl-function
                (lambda (&rest _args)
                  (doge-mcp--show-progress (format "%s completed" tool-name))
                  (condition-case err
                      (let ((response (json-read-from-string (buffer-string))))
                        (funcall callback (assoc-default 'result response)))
                    (error
                     (funcall callback nil)
                     (message "MCP JSON parse error: %s\nResponse: %s" err (buffer-string))))))
      :error (cl-function
              (lambda (&rest _args)
                (doge-mcp--show-progress (format "%s failed" tool-name))
                (funcall callback nil)
                (message "MCP call failed: %s" (buffer-string)))))))

(defun doge-mcp-list-tools ()
  "List available MCP tools."
  (interactive)
  (doge-mcp--show-progress "Listing tools...")
  (doge-mcp--call-tool "list_tools" nil
                       (lambda (result)
                         (if result
                             (message "Available tools: %s" (mapconcat #'identity (assoc-default 'tools result) ", "))
                           (message "Failed to list tools")))))

;;;###autoload
(defun doge-mcp-search-repomap (keywords)
  "Search repomap with KEYWORDS."
  (interactive (list (read-string "Keywords: ")))
  (let ((params `(("keyword_search" . ,(if (string-empty-p keywords) " " keywords)))))
    (doge-mcp--call-tool "search_repomap" params
                         (lambda (result)
                           (if result
                               (with-current-buffer (get-buffer-create "*doge-mcp-output*")
                                 (erase-buffer)
                                 (insert (format "Search Results for '%s':\n\n" keywords))
                                 (dolist (item result)
                                   (let ((file (assoc-default 'file item))
                                         (symbols (assoc-default 'symbols item)))
                                     (insert (format "File: %s\n" file))
                                     (dolist (symbol symbols)
                                       (insert (format "  %s (%s:%d-%d)\n"
                                                       (assoc-default 'name symbol)
                                                       (assoc-default 'kind symbol)
                                                       (assoc-default 'start_line symbol)
                                                       (assoc-default 'end_line symbol)))
                                       (let ((snippet (assoc-default 'code_snippet symbol)))
                                         (when snippet
                                           (insert (format "    %s\n" snippet))))
                                     (insert "\n")))
                                 (display-buffer (current-buffer)))
                             (message "Search failed"))))))

;;;###autoload
(defun doge-mcp-fs-read (path)
  "Read file at PATH via MCP."
  (interactive (list (read-file-name "Path: ")))
  (let ((params `(("path" . ,path))))
    (doge-mcp--call-tool "fs_read" params
                         (lambda (result)
                           (if result
                               (with-current-buffer (get-buffer-create "*doge-mcp-output*")
                                 (erase-buffer)
                                 (insert (assoc-default 'content result))
                                 (display-buffer (current-buffer)))
                             (message "Failed to read file"))))))

;; Bind in doge-code-mode
(with-eval-after-load 'doge-code
  (define-key doge-code-mode-map (kbd "C-c d m s") 'doge-mcp-search-repomap)
  (define-key doge-code-mode-map (kbd "C-c d m f") 'doge-mcp-fs-read)
  (define-key doge-code-mode-map (kbd "C-c d m l") 'doge-mcp-list-tools))

(provide 'doge-mcp)

;;; doge-code-mcp.el ends here