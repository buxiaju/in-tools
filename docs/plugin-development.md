# InTools 插件开发手册

> 适用版本：InTools v0.4.0 · 宿主协议版本 1.0

InTools 插件是一个独立子进程，通过 stdin/stdout 与宿主交换 JSON-RPC 2.0 消息。用什么语言写都行，只要能读写标准输入输出。

本手册覆盖：清单字段、四种插件形态、通信协议、快捷键、插件界面、配置存储、说明文档、MCP 集成、打包分发与调试。

---

## 一、先跑起来最小的那个

新建 `plugins/my-plugin/`，放两个文件。

`manifest.toml`：

```toml
[plugin]
id = "com.example.myplugin"
name = "我的插件"
version = "0.1.0"

[exec]
command = "python"
args = ["-u", "main.py"]

[[tools]]
name = "my:echo"
description = "原样返回传入的文本"
[tools.input_schema]
type = "object"
required = ["text"]
[tools.input_schema.properties.text]
type = "string"
description = "要回显的文本"

[capabilities]
permissions = []
```

`main.py` 用后面第九章的骨架直接抄。

重启 InTools，插件页就能看到它。**注册表是启动时的快照**，所以新增或改动 manifest 都要重启才生效（改 Python 代码不用，插件是按需拉起的进程）。

`args` 里的 `-u` 不能省。Python 对管道默认带缓冲，少了它宿主收不到任何响应，表现为所有调用超时。

---

## 二、目录结构

```
plugins/my-plugin/
├── manifest.toml   # 清单：唯一必需的文件
├── main.py         # 入口，也可以是 index.js、编译好的二进制
└── README.md       # 可选：使用说明，由 [docs] 引用
```

目录名可以随便起，宿主认的是 `manifest.toml` 里的 `id`，不要求目录名和 id 一致。

---

## 三、插件的四种形态

快捷键和界面**都是可选的**，你按需要挑。四种组合都是合法且完整的插件：

| 形态 | 声明 | 典型场景 | 仓库里的例子 |
| --- | --- | --- | --- |
| 只有工具 | 只写 `[[tools]]` | 给 AI 用的能力，用户不直接操作 | `file-search`、`hello-plugin` |
| 工具 + 快捷键 | 加 `[shortcut]`（不写 `ui`） | 一键执行，不需要用户填参数 | — |
| 工具 + 快捷键 + 界面 | 加 `[shortcut]` 且写 `ui` | 执行前需要用户在屏幕上指定东西 | `screenshot-plugin` |
| 工具 + 设置项 | 加 `[[settings]]` | 有需要用户长期保存的参数 | `screenshot-plugin` |

没声明的能力，宿主界面上就不会出现对应入口——插件卡片上不会多出「快捷键」按钮，设置弹窗会是空的。**不声明不是残缺，是正常选择。**

---

## 四、manifest.toml 字段总表

### 必需的部分

| 段 | 字段 | 类型 | 必填 | 说明 |
| --- | --- | --- | --- | --- |
| `[plugin]` | `id` | string | 是 | 反向域名格式，如 `com.example.myplugin`。全局唯一，改了等于换了个插件 |
| | `name` | string | 是 | 界面上显示的名字 |
| | `version` | string | 是 | 语义化版本号 |
| | `author` | string | 否 | 作者 |
| | `description` | string | 否 | 一句话描述 |
| `[exec]` | `command` | string | 是 | 启动命令，如 `python`、`node`、`./my-binary` |
| | `args` | string[] | 否 | 命令参数。Python 务必带 `-u` |
| | `env` | table | 否 | 追加的环境变量 |
| `[[tools]]` | `name` | string | 是 | 工具名，用 `插件:动作` 格式，如 `search:files` |
| | `description` | string | 是 | 工具描述。**AI 靠这句话决定要不要调它**，写清楚 |
| | `[tools.input_schema]` | JSON Schema | 是 | 参数 schema，标准 JSON Schema |
| `[capabilities]` | `permissions` | string[] | 否 | 权限声明，见下 |
| `[lifecycle]` | `mode` | string | 否 | `on-demand`（默认）/ `background` / `startup` |
| | `idle_timeout_sec` | integer | 否 | 空闲多久回收进程，默认 300。不接受 0 |
| | `restart_policy` | string | 否 | `never` / `on-failure`（默认）/ `always` |
| | `request_timeout_sec` | integer | 否 | 单次请求超时秒数，默认 30 |

`mode = "startup"` 的插件随 InTools 启动常驻，不会空闲回收——只有像 AI 编排那种每次对话都要用的才值得这样，否则白占内存。这时 `idle_timeout_sec` 填个大数字表达「不过期」即可（校验不接受 0）。

### 权限表

格式：`category:action[:scope]`。`category` 与 `action` 必须是下表里的合法组合，否则
manifest 解析时报错并拒收。`scope` 是自由字符串，常见值有具体路径
（`file:read:~/Pictures`）、域名（`network:http:api.example.com`）或通配（`*`）。

**类别**（19 个面，每个面只能配下表列出的动作）：

| 类别 | 可用动作 | 说明 |
| --- | --- | --- |
| `file` | `read` / `write` | 文件系统读写 |
| `network` | `http` / `websocket` / `dns` / `socket` / `read` / `write` / `send` / `receive` | 网络访问 |
| `process` | `spawn` | 启动子进程 |
| `shell` | `exec` | 通过 cmd / PowerShell / bash 执行命令 |
| `screen` | `capture` / `record` | 屏幕截图与录屏 |
| `input` | `control` | 模拟键鼠输入 |
| `clipboard` | `read` / `write` | 读写剪贴板 |
| `audio` | `capture` / `record` / `send` / `receive` | 麦克风与扬声器 |
| `system` | `manage` | 关机 / 重启 / 注销 / 锁屏 |
| `window` | `manage` / `modify` | 操纵其他窗口（最小化 / 关闭 / 置顶 / 移动） |
| `app` | `spawn` | 启动其他桌面应用 |
| `registry` | `read` / `write` / `modify` | Windows 注册表读写 |
| `credential` | `read` / `write` | 凭据存储访问 |
| `crypto` | `read` / `write` | 密钥 / 证书访问 |
| `notification` | `send` | 发送系统通知 |
| `hardware` | `read` / `write` / `control` | 摄像头 / 蓝牙 / 串口等通用硬件 |
| `persistence` | `install` / `uninstall` / `modify` | 安装 / 卸载 / 自启动等持久化行为 |
| `schedule` | `manage` | 计划任务 / cron 表达式注册 |
| `environment` | `read` / `write` | 进程环境变量 |

**危险度规则**：

- **高危**（每会话首次使用时都问）：`input:control`、`process:spawn`、`shell:exec`、
  `app:spawn`、`persistence:install/uninstall`、`hardware:control`、`audio:record`、
  `network:socket`、以及「`*`」升档后的 `file:write:*` / `file:read:*` /
  `process:spawn:*` / `shell:exec:*`。
- **中危**（首次使用时问）：`file:write`、`screen:capture/record`、`clipboard:read/write`、
  `audio:capture`、`window:manage/modify`、`registry:read/write/modify`、
  `credential:read/write`、`crypto:read/write`、`persistence:modify`、`hardware:read/write`、
  `environment:write`、以及「`*`」升档后的 `network:http:*` / `network:websocket:*` /
  `network:dns:*`。
- **低危**（声明即生效）：`file:read`、`network:http/websocket/dns`、
  `notification:send`、`environment:read` 等。

低危声明即生效；中危与高危首次使用时问用户，高危每次会话再问一次。
「`file:write:*`」「`network:http:*`」这类「scope = `*`」会把基础危险度提一档——
任意主机 / 任意路径是「默认允许的子集 = 全集」的最危险情形。

**按最小需要声明。** 多声明一个高危权限，用户就多一次弹窗、多一分犹豫。用户的授权决定存在 `~/.intools/permissions.json`，被拒过的可以在权限页删掉记录重新询问。

### 工具名命名

manifest 里写原始形式 `search:files`。暴露给 MCP 客户端时宿主自动转成合法标识符（`:` → `_`，即 `search_files`）。插件内部处理 `tools/call` 时**始终收到原始名**，不用管转换。

---

## 五、`[shortcut]`：全局快捷键

整张表可选。声明了，用户就能不打开主窗口、不经过 AI，一键触发某个工具。

```toml
[shortcut]
key = "Ctrl+Shift+S"      # 可选：默认键位
tool = "screenshot:capture"  # 必填：按下时调用哪个工具
ui = "region-select"      # 可选：先弹交互界面
```

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `key` | 否 | 默认键位。**允许为空串**，表示「支持热键但不预设按键」，等用户自己绑 |
| `tool` | 是 | 必须是本插件 `[[tools]]` 里声明过的工具名，否则清单校验直接失败 |
| `ui` | 否 | 交互界面类型。支持三种：`"region-select"`（框选区域）、`"color-picker"`（屏幕取色）、`"clipboard-history"`（剪贴板历史面板） |

### 键位格式

修饰键顺序**必须**是 `Ctrl` → `Alt` → `Shift` → `Super`，主键在最后。这不是风格偏好，是底层 `global-hotkey` 解析器的硬性要求，写成 `Shift+Ctrl+S` 会注册失败。

主键可用：字母 `A`–`Z`、数字 `0`–`9`、`F1`–`F24`、方向键 `Up/Down/Left/Right`，以及 `Space` `Enter` `Tab` `Backspace` `Delete` `Insert` `Home` `End` `PageUp` `PageDown` `Comma` `Period` `Slash` `Semicolon` `Quote` `Backquote` `Minus` `Equal` `BracketLeft` `BracketRight`。

**至少要带一个修饰键。** 裸键会把这个按键从整个系统里抢走，宿主不接受。

### `key` 只是默认值

用户可以在插件卡片上点「快捷键」改键，改动存进 `~/.intools/shortcuts.json`，优先级高于 manifest，**不会回写你的插件目录**。用户也能点「恢复默认」退回你预设的键位。改键立即生效，不需要重启。

所以：挑一个不容易撞车的默认键就行，撞了用户自己会改。宿主检测到冲突时会在设置页标出来，并告诉用户是被哪个插件占了。

### 快捷键路径拿不到参数

没有 `ui` 时，按下热键就直接调工具。**这条路径上没有界面可以填参数**，宿主会把这个插件 `[[settings]]` 的当前值整体作为 `arguments` 传进去。

这决定了一件事：**想让工具能被快捷键有意义地触发，它的必填参数要么没有，要么能从 `[[settings]]` 里取到。** 一个 `required` 里塞了三个字段却没对应设置项的工具，绑了快捷键也只会失败。

---

## 六、`ui`：插件自己的界面

`ui = "region-select"` 时，按下快捷键的流程变成：

```
用户按下热键
   → 宿主打开全屏透明覆盖层（overlay 窗口）
   → 用户拖拽框选区域，或直接点一下表示全屏，Esc 放弃
   → 覆盖层先把自己藏掉，再调用你的工具，携带 region 参数
   → 覆盖层关闭
```

工具会收到：

```json
{"region": {"x": 100, "y": 200, "width": 800, "height": 600}}
```

有两个坑写在这里，是踩过的：

**坐标是物理像素，不是 CSS 逻辑像素。** 覆盖层已经乘过 `devicePixelRatio` 再取整。你在插件里直接按物理像素处理（Windows 上记得 `SetProcessDPIAware`），不要再换算一次。少了这步，150% 缩放的屏幕上选区会整体偏移并缩小三分之一。

**截屏类工具要注意覆盖层的存在。** 覆盖层在调用工具前会先 `hide()` 并等两帧加 60 毫秒，就是为了不把半透明遮罩和选区边框一起拍进照片。你自己实现同类界面时得记着这件事。

**`ui` 的值不做白名单校验。** 写错字符串（比如 `region_select`）不会报错，宿主只在日志里留一条 warn，然后**降级成不带界面的直接调用**。也就是说插件核心功能还在，只是界面没了——排查时先去 `~/.intools/logs/` 看有没有「未知的 ui 类型」。

---

## 七、`[docs]`：使用说明

整表可选。声明了，插件卡片上会多一个「使用说明」按钮。

```toml
[docs]
usage_file = "README.md"
```

`usage_file` 是**插件根目录下的纯文件名**，不是路径。

### 渲染支持的 Markdown 子集

宿主内置一个极简渲染器，支持：标题（`#`～`###`，更深的按三级显示）、段落、无序列表（`-` `*` `+`）、表格（GFM 管道语法，要带分隔行）、围栏代码块（```）、行内代码（反引号）、分隔线（`---`）。

**不支持**：图片、链接跳转、嵌套列表、有序列表编号、加粗斜体。

之所以砍这么狠：这个前端没有打包器装不了 Markdown 库，而 README 是你——插件作者——提供的内容，对宿主来说属于不可信输入。渲染器全程只用 `textContent` 建节点，从根上没有脚本注入的入口。代价就是花样少。写说明时按上面的子集来，多余语法会原样显示成文本。

### 说明里值得写什么

参考 `plugins/screenshot-plugin/README.md` 和 `plugins/hello-plugin/README.md`。经验上这几项最有用：

- 怎么用（快捷键怎么按、对话里怎么说）
- 结果去哪了（文件存哪、命名规则）
- 要哪些权限、为什么要
- 有什么限制（做不到什么，比这个插件能做什么更重要）
- 出问题先看哪里

---

## 八、`[[settings]]`：用户可配置项

整段可选。声明了，插件卡片上的「设置」按钮里会出现对应表单。

```toml
[[settings]]
key = "output_dir"
label = "截图保存目录"
type = "string"
default = "~/Pictures"

[[settings]]
key = "image_format"
label = "图片格式"
type = "select"
default = "png"
options = ["png", "jpg"]
```

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `key` | 是 | 字段名，非空且在本插件内唯一。它会成为工具参数里的键名 |
| `label` | 是 | 表单上显示的文字，非空 |
| `type` | 是 | `string` / `number` / `boolean` / `select` / `color` / `path` / `password` |
| `default` | 否 | 默认值 |
| `options` | select 必填 | 下拉选项，非空数组 |
| `group` | 否 | string | 字段分组标题，相同 group 的字段归入同一视觉区域 |
| `description` | 否 | string | 字段下方的说明文字 |
| `placeholder` | 否 | string | 输入框占位提示文本 |
| `min` | 否 | float | number 类型的最小值 |
| `max` | 否 | float | number 类型的最大值 |
| `step` | 否 | float | number 类型的步长 |

### 设置值是怎么到你手上的

**通过工具调用参数，不是通过 `host/getConfig`。**

调用工具时，宿主把这个插件的设置值**垫在显式参数下面**：显式传了的字段用显式值，没传的用设置值。所以 `screenshot:capture` 实际收到的是 `{output_dir, image_format, region}` —— 前两个来自设置，`region` 来自框选界面。

你在插件里就当普通参数读：

```python
output_dir = arguments.get("output_dir") or "~/Pictures"
```

**这是最容易踩的一个坑：** `host/getConfig` 读不到 `[[settings]]` 的值，永远返回你自己写过的那份数据。两者是磁盘上两个不同的文件，见下一章。

---

## 九、两套配置存储，别搞混

| | `[[settings]]` | `host/getConfig` / `host/setConfig` |
| --- | --- | --- |
| 文件 | `~/.intools/plugin-settings/<id>.json` | `~/.intools/plugin-configs/<id>.json` |
| 谁写 | 用户在设置界面填 | 插件自己在运行时写 |
| 谁读 | 宿主读，合并进工具参数 | 插件自己读 |
| 用户看得见吗 | 看得见、能改 | 看不见 |
| 适合放什么 | 用户要调的偏好：目录、格式、开关 | 插件的运行状态：缓存、上次结果、令牌 |

用错的表现很典型：声明了 `[[settings]]`，然后在插件里用 `host/getConfig` 去读，结果永远是 `{}`。

另外 `host/setConfig` 是**整体覆盖**，不是按键合并。想改一个字段，先 `getConfig` 拿全量，改完再整份写回去。

---

## 十、通信协议

宿主与插件通过 stdin/stdout 交换 JSON-RPC 2.0 消息，**每条消息一行**，以 `\n` 结尾。

**stdout 只允许出现协议消息。** 任何调试输出必须走 stderr，否则会污染协议链路——这是新手第二常见的故障源（第一是忘了 `-u`）。

### 启动握手

```
宿主                         插件
  │                           │
  ├── spawn subprocess ──────►│
  │◄── plugin/hello ─────────┤   通知：{protocol_version, tools}
  ├── plugin/ready ──────────►│   请求：{config, plugin_dir}
  │◄── result ───────────────┤   响应：{ok: true}
  │   ◄── 正常 RPC 通信 ────► │
```

插件被 spawn 后必须**在 10 秒内**主动发出 `plugin/hello`，否则判定启动失败。

### 宿主 → 插件

| 方法 | 用途 | params |
| --- | --- | --- |
| `plugin/ready` | 握手第二步，传入插件目录和配置 | `{plugin_dir, config}` |
| `tools/list` | 请求工具清单 | `{}` |
| `tools/call` | 调用工具 | `{name, arguments}` |
| `plugin/shutdown` | 通知插件退出 | `{}` |

`plugin/ready` 里的 `plugin_dir` 是你的插件目录绝对路径。要读随插件分发的数据文件（词典、模板、模型），用它拼路径，不要依赖当前工作目录。

### 插件 → 宿主（响应）

成功：

```json
{"jsonrpc":"2.0","id":2,"result":{"text":"你好"}}
```

失败：

```json
{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"参数缺失"}}
```

常用错误码：`-32601` 方法不存在、`-32602` 参数无效、`-32603` 内部错误、`-32000` 自定义业务错误。

错误 `message` 会原样展示给用户，也会被 AI 看到。写「参数 keyword 缺失」而不是「error」——前者 AI 能据此重试，后者只能放弃。

### 插件 → 宿主（通知，无 id）

```json
{"jsonrpc":"2.0","method":"notify/progress","params":{"progress":50}}
```

| 方法 | 用途 |
| --- | --- |
| `notify/progress` | 进度更新 |
| `notify/stream` | 流式输出片段 |
| `notify/toolCall` | 工具调用状态 |
| `notify/streamEnd` | 流式输出结束 |

### 反向 RPC（插件主动请求宿主）

| 方法 | 用途 | params |
| --- | --- | --- |
| `host/listTools` | 获取所有已启用工具及 Schema | `{}` |
| `host/callTool` | 调用其他插件的工具 | `{name, arguments}` |
| `host/getConfig` | 读取本插件**私有**存储（不是 `[[settings]]`） | `{}` |
| `host/setConfig` | 覆盖写入本插件私有存储 | `{key: value, ...}` |
| `host/notify` | 向 UI 推送消息 | `{method, params}` |

反向 RPC 的请求带 `id`，宿主会回对应响应。**等响应期间宿主可能发来新的 `tools/call`（比如递归调用），插件必须就地处理，不能死等。** 调用链深度上限 5 层。

`host/listTools` 只返回**已启用**插件的工具，被用户禁用的不在其中——AI 说「没有这个工具」时先去插件页看开关。

---

## 十一、Python 骨架

最小可运行插件，直接复制：

```python
"""插件说明。"""
import json
import sys

PROTOCOL_VERSION = "1.0"

TOOLS = [
    {
        "name": "my:tool",
        "description": "工具描述",
        "input_schema": {
            "type": "object",
            "required": ["input"],
            "properties": {
                "input": {"type": "string", "description": "输入文本"}
            },
        },
    },
]

CODE_METHOD_NOT_FOUND = -32601
CODE_INVALID_PARAMS = -32602


def log(message):
    """写 stderr。宿主会把 stderr 全量转存到日志文件。"""
    print(message, file=sys.stderr, flush=True)


def write_message(payload):
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def notify(method, params):
    write_message({"jsonrpc": "2.0", "method": method, "params": params})


def reply_result(request_id, result):
    write_message({"jsonrpc": "2.0", "id": request_id, "result": result})


def reply_error(request_id, code, message):
    write_message({"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}})


def call_tool(params):
    """执行工具调用，返回 (result, error)，两者恰有一个非 None。"""
    name = params.get("name")
    arguments = params.get("arguments") or {}

    if name == "my:tool":
        text = arguments.get("input")
        if not isinstance(text, str):
            return None, (CODE_INVALID_PARAMS, "参数 input 缺失或不是字符串")
        return {"output": text}, None

    return None, (CODE_METHOD_NOT_FOUND, "未知工具：{}".format(name))


def handle_request(request_id, method, params):
    if method == "plugin/ready":
        reply_result(request_id, {"ok": True})
        return False

    if method == "tools/list":
        reply_result(request_id, {"tools": TOOLS})
        return False

    if method == "tools/call":
        result, error = call_tool(params)
        if error is None:
            reply_result(request_id, result)
        else:
            reply_error(request_id, error[0], error[1])
        return False

    if method == "plugin/shutdown":
        reply_result(request_id, {"ok": True})
        return True

    reply_error(request_id, CODE_METHOD_NOT_FOUND, "未知方法：{}".format(method))
    return False


def main():
    # Windows 上 Python 面对管道默认用系统代码页，写中文会抛 UnicodeEncodeError。
    # 钉死 UTF-8 消除环境依赖。
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")

    # 握手第一步：主动通知宿主。
    notify("plugin/hello", {"protocol_version": PROTOCOL_VERSION, "tools": TOOLS})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            message = json.loads(line)
        except ValueError:
            log("丢弃无法解析的输入行：{}".format(line[:200]))
            continue

        request_id = message.get("id")
        method = message.get("method")

        if method is None:
            # 响应消息（反向 RPC 的回复），主循环忽略。
            continue

        if request_id is None:
            log("忽略通知：{}".format(method))
            continue

        if handle_request(request_id, method, message.get("params") or {}):
            break

    log("插件退出")


if __name__ == "__main__":
    main()
```

`TOOLS` 里的定义要和 manifest 的 `[[tools]]` 对得上。manifest 决定宿主注册什么，`tools/list` 决定插件承认自己能干什么，两边不一致会出现「调用了却说未知工具」。

---

## 十-b、Node.js 骨架

InTools 自带 Node.js SDK（`plugins/node-sdk/intools.js`），封装了协议握手、消息编解码、工具路由、反向 RPC、进度通知，插件只关注业务逻辑。

```js
// manifest.toml: command = "node", args = ["main.js"]
const { Plugin } = require("../node-sdk/intools");

const plugin = new Plugin("1.0");

plugin.tool(
  "my:echo",                     // 工具名
  "原样返回文本",                 // 描述
  {                               // JSON Schema
    type: "object",
    required: ["text"],
    properties: { text: { type: "string" } },
  },
  (args) => ({ text: args.text }) // 处理函数，可返回 Promise
);

plugin.start();
```

SDK 自动处理 `plugin/hello` 握手、`plugin/ready` 确认、`plugin/shutdown` 退出、`tools/list` 与 `tools/call` 路由。工具函数抛出 `PluginError` 即返回错误响应，其他异常自动转为 `-32603 INTERNAL`。

```js
const { Plugin, PluginError, ErrorCode } = require("../node-sdk/intools");
// ...
if (!args.path) throw new PluginError(ErrorCode.INVALID_PARAMS, "path 缺失");
```

### 反向 RPC

handler 的第二个参数是 `ctx`（`ToolContext`），提供跨插件调用能力：

```js
plugin.tool("my:task", "调用其他插件", { ... }, async (args, ctx) => {
  // 列出所有可用工具
  const tools = await ctx.listTools();

  // 调用其他插件的工具
  const ocrResult = await ctx.callTool("ocr:recognize", { image_path: args.path });

  // 读写本插件私有配置
  const config = await ctx.getConfig();
  await ctx.setConfig({ last_run: Date.now() });

  return { ocr: ocrResult };
});
```

### 进度通知

```js
plugin.tool("my:long", "耗时任务", { ... }, async (args, ctx) => {
  ctx.progress({ percent: 0, message: "开始" });
  // ...
  ctx.progress({ percent: 50, message: "处理中" });
  // ...
  ctx.progress({ percent: 100, message: "完成" });

  // 流式文本
  ctx.stream("第一段文本");
  ctx.stream("第二段文本");
});
```

完整示例见 `plugins/system-info/`（基础用法）和 `plugins/workflow-runner/`（反向 RPC + 进度通知）。

---

## 十一-c、Go / 编译型语言骨架

编译型语言不需要 SDK——协议是逐行 JSON，用标准库的 `bufio.Scanner` + `encoding/json` 二十行就能读写。

```go
// manifest.toml: command = "my-tool.exe"
func main() {
    // 握手：主动发 plugin/hello
    notify("plugin/hello", map[string]interface{}{
        "protocol_version": "1.0",
        "tools":            tools,
    })

    scanner := bufio.NewScanner(os.Stdin)
    for scanner.Scan() {
        var msg jsonrpcMsg
        json.Unmarshal([]byte(scanner.Text()), &msg)
        // msg.ID != nil && msg.Method != "" → 请求
        // 路由到 tools/call、plugin/shutdown 等
    }
}
```

编译后的 `.exe` 直接放在插件目录，manifest 里 `command = "my-tool.exe"`，无需解释器。

**一次遍历多份哈希**等 CPU 密集型任务，Go 比 Python/Node 快数倍，是编译型插件的典型场景。完整示例见 `plugins/file-hash/`。

---

## 十一-d、其他语言

协议只依赖 stdin/stdout JSON，任何能读写管道的语言都可以做插件：

| 语言 | stdin 读取 | stdout 写入 | 启动命令 |
|------|-----------|------------|---------|
| Python | `for line in sys.stdin` | `print(json.dumps(...))` | `python -u main.py` |
| Node.js | `readline` 或 SDK | `process.stdout.write(...)` | `node main.js` |
| Go | `bufio.Scanner` | `fmt.Println` | `my-plugin.exe` |
| Rust | `std::io::stdin` | `serde_json::to_writer` | `my-plugin.exe` |
| C# | `Console.ReadLine()` | `Console.WriteLine()` | `dotnet run` |
| Java | `BufferedReader(System.in)` | `System.out.println` | `java -jar plugin.jar` |
| Ruby | `STDIN.each_line` | `puts JSON.generate(...)` | `ruby main.rb` |
| Shell | `while read line` | `echo '{"jsonrpc":...}'` | `bash main.sh` |

**唯一约束**：stdout 只能输出协议帧（一行一条 JSON），调试日志必须走 stderr。任何语言都能满足这个条件。

---

## 十二、运行时目录

所有用户数据在 `~/.intools/`：

| 路径 | 内容 |
| --- | --- |
| `plugins/` | 用户导入的插件（仓库自带的示范插件在项目 `plugins/` 下） |
| `cache/tools.json` | 工具清单缓存 |
| `permissions.json` | 用户的权限授予/拒绝记录 |
| `mcp-exposure.json` | 哪些工具对外暴露给 MCP 客户端 |
| `host-config.json` | 宿主自身设置 |
| `shortcuts.json` | 用户改过的快捷键（覆盖 manifest 默认值） |
| `plugin-settings/<id>.json` | `[[settings]]` 的当前值 |
| `plugin-configs/<id>.json` | 插件私有存储（`host/*Config`） |
| `logs/<id>.log` | 插件 stderr 全量转存 |

插件 id 里不能有 `/`、`\`、`.`、`..`，也不能为空——这些会被拒，因为要用作文件名。

---

## 十三、打包与分发

打成 zip 让用户在插件页导入。**只接受两种结构：**

```
方式一（推荐）：manifest.toml 在压缩包根目录
my-plugin.zip
├── manifest.toml
├── main.py
└── README.md

方式二：只有一层包裹目录（Windows 资源管理器右键压缩就是这种）
my-plugin.zip
└── my-plugin/
    ├── manifest.toml
    └── main.py
```

嵌套更深，或者根目录下有多个平级目录，导入会报「找不到 manifest」。

限制：解压后不超过 200 MiB，条目不超过 5000 个。符号链接、路径穿越（`../`）、声明尺寸与实际尺寸不符的压缩炸弹都会被拒绝。

导入成功后提示「重启 InTools 后生效」——注册表是启动时扫描出来的快照。目录名冲突时宿主自动加 `-2`、`-3` 后缀，不会覆盖已有插件。

---

## 十四、调试

### 看 stderr 日志

宿主全量捕获插件 stderr 到 `~/.intools/logs/<plugin-id>.log`：

```python
print("调试信息", file=sys.stderr, flush=True)
```

`flush=True` 别省，崩溃时没刷出去的缓冲就永远看不到了。

### 不启动 InTools，手动模拟协议

开发期最快的回路——不用重启宿主：

```python
import subprocess, json, sys

proc = subprocess.Popen(
    [sys.executable, "-u", "main.py"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    encoding="utf-8", cwd="plugins/my-plugin"
)

def send(msg):
    proc.stdin.write(json.dumps(msg) + "\n")
    proc.stdin.flush()

# 读握手
hello = json.loads(proc.stdout.readline())
print("握手:", hello["method"])

# 发送 plugin/ready
send({"jsonrpc": "2.0", "id": 1, "method": "plugin/ready", "params": {"plugin_dir": "."}})
print("ready:", json.loads(proc.stdout.readline()))

# 调用工具
send({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
    "name": "my:tool",
    "arguments": {"input": "hello"}
}})
print("result:", json.loads(proc.stdout.readline()))
```

要测「设置值垫在参数下面」的行为，就在 `arguments` 里手动带上那些字段——效果等价。

### 常见问题

| 问题 | 原因 | 解决 |
| --- | --- | --- |
| 请求全部超时 | stdout 有缓冲，宿主读不到响应 | 启动参数加 `-u` |
| 中文写入报错 | Windows 默认用系统代码页 | `sys.stdout.reconfigure(encoding="utf-8")` |
| 握手失败 | 10 秒内没发 `plugin/hello` | 启动后第一件事就发握手通知 |
| stdout 污染 | 在 stdout 打印了非协议内容 | 所有调试输出走 stderr |
| 反向 RPC 死锁 | 等响应时不处理宿主请求 | 等待期间继续读 stdin 并处理 |
| 改了 manifest 没反应 | 注册表是启动快照 | 重启 InTools |
| 快捷键不生效 | 键位被别的程序占了 | 设置页会标「未生效」，让用户改键 |
| 快捷键注册失败 | 修饰键顺序错了 | 必须 `Ctrl→Alt→Shift→Super` |
| 界面没弹出来，工具却执行了 | `ui` 值拼错，已降级 | 查日志里的「未知的 ui 类型」 |
| `host/getConfig` 返回 `{}` | 想读 `[[settings]]` 但那是另一套存储 | 从工具 `arguments` 里读 |

---

## 十五、示范插件

仓库 `plugins/` 下的插件，按学习顺序：

| 插件 | 演示内容 |
| --- | --- |
| `hello-plugin` | 最小握手、工具调用、崩溃处理。**没有快捷键、界面、设置项**，示范三者皆可省 |
| `file-search` | 实用工具、标准库实现、跨平台路径处理 |
| `clipboard-tool` | 多工具声明（读/写/追加/清空/检测）、`clipboard:read` / `clipboard:write` 权限 |
| `screenshot-plugin` | `[shortcut]` + `ui = "region-select"` + `[[settings]]` + `[docs]`，功能最全 |
| `color-picker` | 快捷键 + 自定义 UI 类型（`color-picker`）、屏幕取色 + 剪贴板联动 |
| `ocr-tool` | `[shortcut]` + `ui = "region-select"` + `[[settings]]`、Windows OCR API 调用 |
| `window-info` | Win32 API 调用、多工具（活动窗口/列表/搜索）、无权限声明 |
| `ai-orchestrator` | 反向 RPC 编排、`startup` 常驻、`host/*Config` 私有存储 |
| `caller-plugin` | 反向 RPC 各分支、递归调用深度上限（集成测试桩） |
| `responder-plugin` | 权限拦截验证（集成测试桩） |
| `ui-demo` | 插件 UI 声明系统演示：分组表单、增强设置类型（color/path/password）、kv/table/markdown 结果展示 |

想照着改一个能用的，从 `file-search` 开始；想做带快捷键和界面的，直接读 `screenshot-plugin`。

---

## 十六、插件 UI 声明

宿主提供声明式 UI 系统，插件在 manifest.toml 中描述界面，宿主负责渲染。
这让你用任何语言写的插件都能拥有统一的、美观的用户界面。

### 设置字段增强

除原有的 `string` / `number` / `boolean` / `select` 外，现在支持更多类型：

| 类型 | 渲染控件 | 典型用途 |
| --- | --- | --- |
| `string` | 文本框 | 通用文本输入 |
| `number` | 数字框 | 数值参数（支持 min/max/step） |
| `boolean` | 开关 | 功能开关 |
| `select` | 下拉框 | 枚举选项 |
| `color` | 颜色选择器 | 主题色、标记色 |
| `path` | 路径输入框 | 文件/目录路径 |
| `password` | 密码框 | API Key、Token |

#### 分组与描述

字段可以按 `group` 分组，并附带 `description` 说明文字：

```toml
[[settings]]
key = "api_key"
label = "API Key"
type = "password"
group = "API 配置"
description = "在服务商后台获取，格式为 sk-..."

[[settings]]
key = "model"
label = "模型"
type = "select"
default = "deepseek-chat"
options = ["deepseek-chat", "deepseek-coder"]
group = "API 配置"
description = "选择使用的 AI 模型"

[[settings]]
key = "output_dir"
label = "输出目录"
type = "path"
default = "~/output"
group = "输出设置"
placeholder = "选择文件保存位置"
```

同一 `group` 值的字段会归入同一个视觉区域，带分组标题。

### 工具结果展示

插件可以声明工具结果的结构化展示方式，宿主将 JSON 结果渲染为可读的 UI 组件：

```toml
[[result_display]]
tool = "my:status"
type = "kv"
fields = [
  { key = "version", label = "版本" },
  { key = "uptime", label = "运行时间" },
  { key = "memory", label = "内存使用" },
]
```

支持三种展示类型：

| 类型 | 说明 | 适用场景 |
| --- | --- | --- |
| `kv` | 键值对展示 | 状态信息、配置摘要 |
| `table` | 表格展示 | 列表数据、查询结果 |
| `markdown` | Markdown 渲染 | 富文本报告、说明文档 |

另有 `raw` 类型作为降级选项，直接展示原始 JSON 文本。

所有展示类型通用可选字段：

| 字段 | 说明 |
| --- | --- |
| `status_key` | 结果根对象中包含状态信息的字段名，宿主据此显示状态指示 |

#### kv 示例

```toml
[[result_display]]
tool = "color:pick"
type = "kv"
fields = [
  { key = "hex", label = "HEX" },
  { key = "rgb_string", label = "RGB" },
  { key = "hsl_string", label = "HSL" },
]
```

#### table 示例

```toml
[[result_display]]
tool = "search:files"
type = "table"
columns = [
  { key = "name", label = "文件名" },
  { key = "path", label = "路径" },
  { key = "size", label = "大小" },
]
```

#### markdown 示例

```toml
[[result_display]]
tool = "report:generate"
type = "markdown"
content_key = "report"
status_key = "status"
```

### 运行时 UI 更新

插件可以通过通知机制发送结构化 UI 更新：

```python
# 插件发送 kv 结果展示
notify("ui/display", {
    "type": "kv",
    "fields": [
        {"key": "status", "label": "状态"},
        {"key": "progress", "label": "进度"},
    ],
    "data": {"status": "运行中", "progress": "75%"}
})
```

这对于长时运行任务的进度展示特别有用。

---

## 十七、MCP 集成

InTools 内置 MCP（Model Context Protocol）网关，启用后其他 MCP 客户端（如 Claude Desktop、Cursor）可以通过 HTTP 调用你插件的工具。

### 工具暴露

默认所有工具**不暴露**给外部 MCP 客户端。用户在权限页手动勾选要暴露的工具，选择结果存入 `~/.intools/mcp-exposure.json`。

工具名暴露时会自动转换：`:` → `_`，如 `search:files` 变成 `search_files`。**插件内部始终收到原始名**，不需要处理这个转换。

### 网关配置

在设置页开启 MCP 网关，宿主会：
1. 生成一个 Bearer Token（首次开启时自动生成）
2. 在本地启动 HTTP 端点
3. 其他 MCP 客户端配置该地址即可调用已暴露的工具

### 对插件的影响

MCP 集成对插件完全透明——工具调用走的是同一条 `tools/call` 路径。插件不需要做任何适配。

唯一要注意的是：**工具描述写清楚**。MCP 客户端（尤其是 AI）靠 `description` 决定要不要调你的工具，描述含糊会导致工具被忽略或误调。
