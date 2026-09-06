// 文件哈希插件（Go）。
//
// 计算文件的 MD5/SHA1/SHA256 哈希值，展示编译型二进制即插即用。
// 仅依赖 Go 标准库，编译后单文件即可运行。
//
// 编译：go build -o file-hash.exe main.go
package main

import (
	"bufio"
	"crypto/md5"
	"crypto/sha1"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
)

const protocolVersion = "1.0"

// ── 工具描述 ──────────────────────────────────────────────────────────

var tools = []map[string]interface{}{
	{
		"name":        "hash:file",
		"description": "计算单个文件的 MD5/SHA1/SHA256 哈希值",
		"input_schema": map[string]interface{}{
			"type":     "object",
			"required": []string{"path"},
			"properties": map[string]interface{}{
				"path": map[string]interface{}{
					"type":        "string",
					"description": "文件的绝对路径",
				},
			},
		},
	},
	{
		"name":        "hash:dir",
		"description": "递归计算目录下所有文件的 SHA256，用于目录内容校验",
		"input_schema": map[string]interface{}{
			"type":     "object",
			"required": []string{"path"},
			"properties": map[string]interface{}{
				"path": map[string]interface{}{
					"type":        "string",
					"description": "目录的绝对路径",
				},
			},
		},
	},
}

// ── 协议消息 ──────────────────────────────────────────────────────────

type jsonrpcMsg struct {
	JSONRPC string      `json:"jsonrpc"`
	ID      interface{} `json:"id,omitempty"`
	Method  string      `json:"method,omitempty"`
	Params  interface{} `json:"params,omitempty"`
	Result  interface{} `json:"result,omitempty"`
	Error   interface{} `json:"error,omitempty"`
}

func writeMsg(msg jsonrpcMsg) {
	b, _ := json.Marshal(msg)
	fmt.Println(string(b))
}

func log(msg string) {
	fmt.Fprintln(os.Stderr, "[InTools] "+msg)
}

func notify(method string, params interface{}) {
	writeMsg(jsonrpcMsg{JSONRPC: "2.0", Method: method, Params: params})
}

func replyResult(id interface{}, result interface{}) {
	writeMsg(jsonrpcMsg{JSONRPC: "2.0", ID: id, Result: result})
}

func replyError(id interface{}, code int, message string) {
	writeMsg(jsonrpcMsg{
		JSONRPC: "2.0",
		ID:      id,
		Error:   map[string]interface{}{"code": code, "message": message},
	})
}

// ── 工具实现 ──────────────────────────────────────────────────────────

func hashFile(path string) (interface{}, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, fmt.Errorf("无法打开文件: %w", err)
	}
	defer f.Close()

	md5h := md5.New()
	sha1h := sha1.New()
	sha256h := sha256.New()

	// 一次遍历同时算三个哈希，避免读三遍文件。
	w := io.MultiWriter(md5h, sha1h, sha256h)
	n, err := io.Copy(w, f)
	if err != nil {
		return nil, fmt.Errorf("读取文件失败: %w", err)
	}

	return map[string]interface{}{
		"path":     path,
		"size":     n,
		"md5":      hex.EncodeToString(md5h.Sum(nil)),
		"sha1":     hex.EncodeToString(sha1h.Sum(nil)),
		"sha256":   hex.EncodeToString(sha256h.Sum(nil)),
		"filename": filepath.Base(path),
	}, nil
}

func hashDir(dir string) (interface{}, error) {
	var files []map[string]interface{}
	err := filepath.Walk(dir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return nil // 跳过无法访问的文件，不阻断整个目录
		}
		if info.IsDir() {
			return nil
		}
		f, err := os.Open(path)
		if err != nil {
			return nil
		}
		defer f.Close()

		h := sha256.New()
		n, err := io.Copy(h, f)
		if err != nil {
			return nil
		}

		rel, _ := filepath.Rel(dir, path)
		files = append(files, map[string]interface{}{
			"path":   rel,
			"size":   n,
			"sha256": hex.EncodeToString(h.Sum(nil)),
		})
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("遍历目录失败: %w", err)
	}

	totalSize := int64(0)
	for _, f := range files {
		totalSize += f["size"].(int64)
	}

	return map[string]interface{}{
		"directory":  dir,
		"file_count": len(files),
		"total_size": totalSize,
		"files":      files,
	}, nil
}

// ── 请求处理 ──────────────────────────────────────────────────────────

func handleRequest(id interface{}, method string, params map[string]interface{}) {
	switch method {
	case "plugin/ready":
		log(fmt.Sprintf("握手完成，插件目录：%v", params["plugin_dir"]))
		replyResult(id, map[string]interface{}{"ok": true})

	case "tools/list":
		replyResult(id, map[string]interface{}{"tools": tools})

	case "tools/call":
		name, _ := params["name"].(string)
		args, _ := params["arguments"].(map[string]interface{})
		if args == nil {
			args = map[string]interface{}{}
		}

		switch name {
		case "hash:file":
			path, _ := args["path"].(string)
			if path == "" {
				replyError(id, -32602, "参数 path 缺失")
				return
			}
			result, err := hashFile(path)
			if err != nil {
				replyError(id, -32000, err.Error())
				return
			}
			replyResult(id, result)

		case "hash:dir":
			path, _ := args["path"].(string)
			if path == "" {
				replyError(id, -32602, "参数 path 缺失")
				return
			}
			result, err := hashDir(path)
			if err != nil {
				replyError(id, -32000, err.Error())
				return
			}
			replyResult(id, result)

		default:
			replyError(id, -32601, "未知工具: "+name)
		}

	case "plugin/shutdown":
		replyResult(id, map[string]interface{}{"ok": true})
		os.Exit(0)

	default:
		replyError(id, -32601, "未知方法: "+method)
	}
}

// ── 主循环 ────────────────────────────────────────────────────────────

func main() {
	// 握手：主动发 plugin/hello
	notify("plugin/hello", map[string]interface{}{
		"protocol_version": protocolVersion,
		"tools":            tools,
	})

	// 读 stdin，逐行处理
	scanner := bufio.NewScanner(os.Stdin)
	// 增大缓冲区：哈希结果可能很长。
	scanner.Buffer(make([]byte, 0, 64*1024), 256*1024)

	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}

		var msg jsonrpcMsg
		if err := json.Unmarshal([]byte(line), &msg); err != nil {
			log(fmt.Sprintf("丢弃无法解析的输入行: %s", line[:min(len(line), 200)]))
			continue
		}

		// 通知（无 id）忽略
		if msg.ID == nil && msg.Method != "" {
			continue
		}
		// 响应（无 method）忽略
		if msg.Method == "" {
			continue
		}

		params, _ := msg.Params.(map[string]interface{})
		if params == nil {
			params = map[string]interface{}{}
		}
	handleRequest(msg.ID, msg.Method, params)
	}

	log("插件退出")
}
