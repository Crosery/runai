# Setup

新人从零上手 runai。所有命令直接复制粘贴。

## 装客户端 / 配置 hook

远程用户没装 runai binary,curl 一行装好(自动写 `~/.runai-identity`、配 Claude Code UserPromptSubmit hook)。

```bash
curl -fsSL http://<server>:<port>/install | bash
```

非交互(免 TTY,适合脚本):

```bash
curl -fsSL http://<server>:<port>/install | RUNAI_USERNAME=alice RUNAI_PASSWORD='your-pw' bash
```

## 装 skill 到公共池(CLI)

本机有 runai binary,装到所有人共享的公共池。

```bash
runai install owner/repo
runai install owner/repo@branch
```

## 装 skill 到「我的库」(仪表板)

仪表板顶栏 → Market → 搜索或浏览 → 点「安装到我的库」。落到当前登录用户的私有池,自动订阅到「我的库」。

## 移出 / 卸载

仪表板 Library → 进入选中模式 → 勾 skill → 点「移出我的库」(只是取消订阅,skill 本体不动)。

软删除到垃圾桶(可恢复):

```bash
runai uninstall <name>
```

垃圾桶管理:

```bash
runai trash list
runai trash restore <name>
runai trash purge <name>
runai trash empty
```

## 上传 skill 到社区市场(team 模式)

本机 binary:

```bash
runai community upload --path ./my-skill --name my-skill
```

远程用户(install 脚本会在 `~/.local/bin/` 放一个 `runai-client`):

```bash
runai-client upload                                    # TUI,fzf 选 skill
runai-client upload --path ./my-skill --name my-skill  # 非交互
```

浏览社区池并装到自己私有池:

```bash
runai community list --sort installs
runai community install <uploader_uid> <skill_name>
```

## 装失败排错

仓库 SKILL.md 不在默认探测路径(`<name>/SKILL.md`、`skills/<name>/SKILL.md`、`agent-skills/<name>/SKILL.md`、根 `SKILL.md`),或 CDN 镜像全 404。

```bash
# 跳过 jsdelivr / ghfast.top,直连 raw.githubusercontent
RUNAI_GH_MIRROR=raw runai install owner/repo
```

```bash
# 私有仓库 / GitHub API 限流(无 token 60 req/h)
GITHUB_TOKEN=ghp_xxx runai install owner/repo
```

```bash
# 验证仓库 SKILL.md 真的存在
curl -I https://raw.githubusercontent.com/owner/repo/main/SKILL.md
```

```bash
# 刷新市场缓存
runai market --search <skill-name>
```

## 三个池子的区别

- **公共池** — 所有人可见,`runai install` 默认落点,本机 `~/.claude/skills/` 有 symlink。
- **私有池** — 仅本人可见,仪表板 Market 安装的落点,跨用户隔离。
- **我的库** — 订阅列表,`/recommend` hook 默认只在这里挑候选。

让 hook 在公共池里也能选:仪表板 Settings → 我的偏好 → 打开 `allow_public_recommend`。
