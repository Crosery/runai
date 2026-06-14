# Setup

新人从零上手 runai。命令直接复制粘贴。

<!-- runai:user-only-start -->

<!-- runai:os-posix-only-start -->

## 装客户端 + 配 hook(Mac / Linux)

把你这台 Claude Code 接到 runai 服务器,自动写 `~/.runai-identity` + 配 UserPromptSubmit hook。脚本会先去 `~/.runai-identity` 复用 api_key,没有再 prompt 注册 / 登录。

首次装(会提示输入用户名 + 密码):

```bash
bash <(curl -fsSL {SERVER_URL}/install)
```

非交互(CI / 脚本 / 已有账号):

```bash
RUNAI_USERNAME=your-name RUNAI_PASSWORD='your-pw' bash <(curl -fsSL {SERVER_URL}/install)
```

重装 / 换机(`~/.runai-identity` 还在直接复用):

```bash
bash <(curl -fsSL {SERVER_URL}/install)
```

> 提示:不要用 `curl ... | bash` 形式 — pipe 占用 stdin 后 prompt 拿不到键盘,会报 `username cannot be empty`。如果一定要用 pipe,必须配 `RUNAI_USERNAME=` / `RUNAI_PASSWORD=` 环境变量。

<!-- runai:os-posix-only-end -->

<!-- runai:os-windows-only-start -->

## 装客户端 + 配 hook(Windows / PowerShell)

把你这台 Claude Code 接到 runai 服务器,自动写 `~\.runai-identity` + 配 UserPromptSubmit hook。脚本会先复用 `~\.runai-identity` 的 api_key,没有再 prompt 注册 / 登录。

首次装(会提示输入用户名 + 密码):

```powershell
iwr -useb {SERVER_URL}/install.ps1 | iex
```

非交互(CI / 脚本 / 已有账号):

```powershell
$env:RUNAI_USERNAME='your-name'; $env:RUNAI_PASSWORD='your-pw'; iwr -useb {SERVER_URL}/install.ps1 | iex
```

重装 / 换机(`~\.runai-identity` 还在直接复用):

```powershell
iwr -useb {SERVER_URL}/install.ps1 | iex
```

> 提示:`iwr` = `Invoke-WebRequest`,`iex` = `Invoke-Expression`。需要 PowerShell 5+(Win10 / Win11 自带)。

<!-- runai:os-windows-only-end -->

## 装 skill 到「我的库」

仪表板顶栏 → Market → 搜索 / 浏览 → 点「安装到我的库」。落到你自己的私有池,自动订阅。

## 移出「我的库」

仪表板 Library → 进入选中模式 → 勾 skill → 点「移出我的库」。只取消订阅,skill 本体仍在,可随时重装。

## 上传 skill 到社区市场

`runai-client` 是 install 脚本装到 `~/.local/bin/` 的客户端,不需要装 runai binary。

```bash
runai-client upload
```

非交互:

```bash
runai-client upload --path ./my-skill --name my-skill
```

浏览社区池 / 装别人上传的 skill:

```bash
runai-client list --sort installs
runai-client install <uploader_uid> <skill_name>
```

## 装失败常见解法

```bash
# CDN 全 404 时跳过镜像,直连 raw.githubusercontent
RUNAI_GH_MIRROR=raw runai-client install <uploader_uid> <skill_name>
```

```bash
# 验证仓库 SKILL.md 真的存在
curl -I https://raw.githubusercontent.com/<owner>/<repo>/main/SKILL.md
```

<!-- runai:user-only-end -->

<!-- runai:admin-only-start -->

## 启动 server

owner 模式(单机自用,免登录):

```bash
runai server --mode owner --host 127.0.0.1 --port 17888
```

team 模式(对外开放,要 TLS):

```bash
runai server --mode team --host 127.0.0.1 --port 17888
runai server --mode team --host 0.0.0.0 --port 17888 --tls-cert ./cert.pem --tls-key ./key.pem
```

## 装 skill 到公共池(所有人共享)

```bash
runai install owner/repo
runai install owner/repo@branch
```

## 移到垃圾桶 / 卸载

仪表板 Library 进选中模式 → 勾 skill → 点红色「移到垃圾桶」(可批量,可恢复)。

CLI 单删:

```bash
runai uninstall <name>
```

## 垃圾桶管理

```bash
runai trash list
runai trash restore <name>
runai trash purge <name>
runai trash empty
```

## 装失败完整排错

```bash
# 跳过 jsdelivr / ghfast.top,直连 raw.githubusercontent
RUNAI_GH_MIRROR=raw runai install owner/repo
```

```bash
# 私有仓库 / GitHub API 限流(无 token 60 req/h,有 token 5000 req/h)
GITHUB_TOKEN=ghp_xxx runai install owner/repo
```

```bash
# 验证仓库 SKILL.md 真的存在
curl -I https://raw.githubusercontent.com/<owner>/<repo>/main/SKILL.md
```

```bash
# 刷新市场缓存(skills.sh leaderboard + sitemap)
runai market --search <skill-name>
```

## admin 管理用户

仪表板顶栏 → Admin → 用户管理(列表 / 启用禁用 / 提升 admin / 删除)。
仪表板 Library → 「用户库」sub-tab → 看每个非 admin 用户的私有 + 导入。

## 升级 binary

```bash
cargo install --path . --force
```

<!-- runai:admin-only-end -->

## 三个池子(所有人都该懂)

- **公共池** — 所有人可见,`runai install` 默认落点,本机 `~/.claude/skills/` 有 symlink。
- **私有池** — 仅本人可见,仪表板 Market 安装的落点,跨用户隔离。
- **我的库** — 订阅列表(只是指针),`/recommend` hook 默认只在这里挑候选。

让 hook 在公共池里也能选:仪表板 Settings → 我的偏好 → 打开 `allow_public_recommend`。
