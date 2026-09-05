# 导入插件包 —— 设计

日期：2026-09-04
状态：待确认

## 1. 目标与边界

让用户通过界面选择一个 `.zip` 插件包，导入到 `~/.intools/plugins/`，重启后生效。

已确认的需求约束：

- 包形态：**`.zip`** **压缩包**（不支持选文件夹）

- 生效时机：**重启后生效**（不做热加载）

明确不做：

- 不做在线插件市场 / 远程下载（实施计划 §286 已列为范围外）

- 不改 `Registry` 的持有方式，不做运行期热加载

- 不做插件签名校验（无签名体系可依托）

- 不做 Python 依赖安装（现有插件全部只用标准库，`manifest.toml` 里也没有依赖声明字段）

## 2. 现状事实（决定设计的关键约束）

以下都是读代码确认过的，不是假设：

1. **插件的身份是** **`plugin.id`，不是目录名。** `file-search/` 目录里的 id 是
   `com.intools.filesearch`。`discovery.rs:10-11` 明确写了两者不要求一致。
2. **重复 id 的现有处理是「后加载者整体被拒绝」**（`registry/mod.rs:92-100`），
   只往 `conflicts` 里记一条。这意味着：**如果导入时不检查 id 冲突，用户会看到
   「导入成功」，重启后插件却静默消失。** 这是最坏的失败模式，必须在落盘前拦住。
3. **`discovery::load_plugin_dir`** **是** **`pub`**（`discovery.rs:125`），注释里就写了
   「公开以便 registry 在热更新单个插件时直接用」。导入的校验直接复用它，不另写
   一套 manifest 解析。
4. **选文件不需要新依赖。** 前端是标准 WebView，`<input type="file" accept=".zip">`
   即可，CSP `script-src 'self'` 不限制 input 元素。无需引入
   `tauri-plugin-dialog`（那会连带改 capabilities 权限配置）。
5. **zip 解压需要新依赖**：`Cargo.toml` 里没有任何压缩相关 crate。
6. `uninstall_plugin`（`commands.rs:230`）是本功能最直接的对照模板：命令签名、
   错误字符串风格、`AppState` 取用方式都照它。

## 3. 方案选择

### 3.1 解压位置：先落暂存区再原子改名

不直接解压到 `~/.intools/plugins/<name>/`。理由是解压中途失败（磁盘满、包损坏、
路径非法）会留下半个插件目录，而下次启动扫描会把它当成一个「缺 manifest」或
「manifest 损坏」的插件报错，用户完全无从理解那是什么。

流程：

```
解压到 ~/.intools/plugins/.import-staging-<uuid>/
  → 校验（找 manifest、解析、查 id 冲突）
  → 通过：rename 到 ~/.intools/plugins/<dir-name>/
  → 任何一步失败：remove_dir_all 暂存目录，返回错误
```

`seed.rs` 用的就是这个模式，此处保持一致。暂存目录放在 plugins 根目录**内部**
而不是系统 temp，是为了保证 rename 在同一卷上——跨卷 rename 在 Windows 上会失败。

前缀 `.import-staging-` 以点开头：`scan_plugins_root` 只跳过普通文件、不跳过点目录，
所以这里不能指望扫描器忽略它。真正的保证是「命令返回前一定清理掉」，点前缀只是
万一进程被强杀时，让残留物一眼能看出是临时产物。

### 3.2 zip 解压的安全处理

这部分是本功能唯一有真实攻击面的地方，逐条列出必须处理的：

| 风险                     | 处理                           |
| ---------------------- | ---------------------------- |
| 路径穿越（条目名含 `../` 或绝对路径） | 逐条目校验规范化后的路径必须仍在暂存目录内，否则整包拒绝 |
| 压缩包炸弹（解压后体积极大）         | 累计解压字节数上限（建议 200 MB），超限即中止   |
| 条目数量爆炸                 | 条目数上限（建议 5000）               |
| 符号链接条目                 | 直接拒绝含符号链接的包                  |

`zip` crate 的 `ZipFile::enclosed_name()` 已经做了穿越检查（返回 `None` 表示不安全），
但仍然显式做一次落盘路径的 `starts_with(staging_dir)` 断言——依赖单一外部检查点
不够稳妥，且这个断言的成本可以忽略。

### 3.3 包结构：允许单层包裹目录

用户打包时的两种自然结果都要接受：

```
A) my-plugin.zip
     manifest.toml
     main.py

B) my-plugin.zip
     my-plugin/
       manifest.toml
       main.py
```

B 是 Windows 资源管理器「压缩到 zip」的默认产物，直接不支持会造成大量困惑。
处理规则：解压后若暂存目录根有 `manifest.toml` 用 A；否则若根下**恰好只有一个
子目录**且该目录含 `manifest.toml`，则以那个子目录为插件根（把它作为 rename 源）；
两者都不满足则报错「包内未找到 manifest.toml」。

不递归深挖多层——超过一层的嵌套说明打包方式确实不对，明确报错比猜测更好。

### 3.4 目标目录名

用 `plugin.id` 的最后一段（`com.intools.filesearch` → `filesearch`）作为目录名，
而不是 zip 文件名。原因：zip 文件名完全不受控（可能是 `新建文件夹 (2).zip`），
而 id 是 manifest 里受校验的字段。若该目录名已被占用但 id 不冲突（罕见但可能），
追加 `-2`、`-3` 直到不占用。

### 3.5 id 冲突处理

因 §2.2 的原因必须拦。采取**直接拒绝并提示先卸载**：

```
插件 `com.intools.filesearch`（文件搜索）已安装，请先卸载后再导入。
```

不做「覆盖」选项。覆盖意味着删除用户现有插件目录，而插件目录里可能有用户自己
放的数据文件；在没有「插件数据目录」概念的当下，静默删掉它风险过高。先卸载再
导入的两步流程虽然繁琐，但每一步的后果都对用户可见。

（这一条如果你倾向做成「询问是否覆盖」，改动只在这一处，不影响其余设计。）

## 4. 实现清单

### 后端

**新增依赖**（`Cargo.toml`）

```toml
# 导入插件包的 zip 解压。default 会拖进 aes / bzip2 / lzma / zstd / xz 等一整套
# 压缩后端，这里全部关掉，只留 deflate 解码——它是 Windows 资源管理器「压缩到
# zip」和常见打包工具的默认算法。用 `deflate-flate2` 而不是 `deflate`：后者是
# 别名，会连带引入 zopfli（纯压缩用的编码器），而本功能只解压、从不写 zip。
zip = { version = "8.6.0", default-features = false, features = ["deflate-flate2"] }
```

同时需把 `[package]` 的 `rust-version` 从 `1.85` 提到 `1.88`：zip 8.6.0 声明的
MSRV 是 1.88。本机工具链是 1.98，实际编译没问题，但把声明留在 1.85 会变成一句
假话——将来有人拿 1.85 来编就会撞上一条指向依赖内部的费解报错。

（若不希望抬 MSRV，可退到 zip 7.x 系列的稳定版；代价是要另行核对其 feature 名，
且本设计其余部分不受影响。）

**新增** **`src-tauri/src/registry/import.rs`**

纯函数、不依赖 Tauri，可独立单元测试：

```rust
pub struct ImportOutcome {
    pub plugin_id: String,
    pub plugin_name: String,
    pub installed_dir: PathBuf,
}

pub enum ImportError {
    NotZip, Corrupt, TooLarge, TooManyEntries,
    UnsafePath { entry: String },
    ManifestMissing,
    ManifestInvalid(String),
    IdConflict { id: String, name: String },
    Io(std::io::Error),
}

/// 把 zip 字节导入到插件根目录。`existing_ids` 由调用方提供。
pub fn import_from_bytes(
    plugins_root: &Path,
    zip_bytes: &[u8],
    existing_ids: &[String],
) -> Result<ImportOutcome, ImportError>;
```

**新增命令**（`commands.rs`，紧邻 `uninstall_plugin`）

```rust
#[tauri::command]
pub async fn import_plugin_package(zip_bytes: Vec<u8>) -> CmdResult<ImportResultView>;
```

现有 id 列表**由磁盘实时扫描得到**（`discovery::scan_plugins_root`），而非原先设想的
`state.supervisor.registry()`。实现时才看清内存注册表是启动时的快照：它既不包含本次
会话里刚导入的插件（连导两次同一个包会都放过去，重启后后者被静默丢弃），也仍包含刚被
卸载的插件（目录已删却拦着不让重装）。磁盘状态才是「重启后会加载成什么」的真相。改用
扫描后命令也就不再需要 `State` 参数。

解压与扫描都是同步 IO，一并放进 `tauri::async_runtime::spawn_blocking`，不阻塞异步
运行时——顺带说明 `uninstall_plugin` 里那个同步 `remove_dir_all` 是既有的同类问题，
本次不动它。

在 `main.rs:72-88` 的 `generate_handler!` 里注册。

### 前端

`index.html` 插件页头部，「刷新」旁加：

```html
<input type="file" id="import-file" accept=".zip" hidden />
<button class="btn" data-act="import-plugin">导入插件包</button>
```

`main.js`：按钮点击触发 `#import-file` 的 `click()`；`change` 事件里读
`file.arrayBuffer()` → `Array.from(new Uint8Array(buf))` 传给命令。成功后提示
「已导入插件《X》，重启 InTools 后生效」，并清空 input 的 value（否则连续导入
同一文件不会再触发 change）。整个过程用现有的 `withBusy` 包起来——一个几 MB 的
包解压有可感延迟，按钮必须置灰。

### 测试

`import.rs` 单元测试（用 `tempfile` + 内存构造 zip）：

测试里构造 zip 用 `ZipWriter` + `CompressionMethod::Stored`（不压缩，直接存储）。
这样即使只开了 `deflate-flate2` 解码 feature 也能写出合法 zip，不需要为测试再加
压缩后端依赖。同时补一个用真实 deflate 压缩的包做解压用例——`Stored` 走的是
另一条代码路径，只测它会漏掉解码逻辑。

1. 扁平结构包 → 成功，目录名取自 id 末段
2. 单层包裹目录包 → 成功
3. 缺 manifest → `ManifestMissing`
4. manifest 语法错误 → `ManifestInvalid`
5. id 与 `existing_ids` 冲突 → `IdConflict`，且**暂存目录已清理、插件根目录无残留**
6. 条目含 `../` → `UnsafePath`，无残留
7. 非 zip 字节 → `Corrupt`
8. 超过体积上限 → `TooLarge`
9. 目标目录名已占用 → 落到 `<name>-2`

第 5、6 条的「无残留」断言是重点：它守护的是 §3.1 那个设计决定。

## 5. 验收

- `cargo test` 全绿（当前 297 项 + 新增约 9 项）

- `cargo clippy --all-targets -- -D warnings` 无输出

- 手工：把 `plugins/hello-plugin` 打包成 zip，先卸载已装的 hello-plugin 并重启，
  再导入该 zip，确认提示正确、目录落在 `~/.intools/plugins/` 下、重启后插件出现
  在列表里并能启动

- 手工负面用例：导入一个随便改名成 `.zip` 的文本文件，确认报错且无残留目录

## 6. 已知取舍

- **重启才生效**：符合已确认需求，与现有卸载行为一致。代价是用户导入后要手动重启。

- **不支持覆盖安装**：升级插件需「卸载 → 重启 → 导入 → 重启」，繁琐。换来的是
  不会误删用户放在插件目录里的文件。

- **不装 Python 依赖**：导入一个依赖第三方库的插件，会在启动时才失败。当前
  `manifest.toml` 没有依赖声明字段，这个问题需要先扩展 manifest 格式才能解决，
  属于独立议题。

- **`spawn_blocking`** **里做完整解压**：一个 200 MB 上限的包在慢盘上可能耗时数秒，
  期间界面按钮置灰但无进度条。加进度需要事件通道，收益不足。

