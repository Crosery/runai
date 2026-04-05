# 搭子 Device Authorization Flow 接口规范

## 背景

Runai (CLI 工具) 需要通过搭子的 team API 发布组合包等操作，目前需要用户手动从浏览器控制台复制 session token。Device Authorization Flow 可以让 CLI 用户零手动操作完成登录。

参考：[RFC 8628 - OAuth 2.0 Device Authorization Grant](https://datatracker.ietf.org/doc/html/rfc8628)，GitHub CLI 也使用相同模式。

## 流程概览

```
┌─────┐                    ┌──────────┐                   ┌─────────┐
│ CLI │                    │ 搭子后端  │                   │  浏览器  │
└──┬──┘                    └────┬─────┘                   └────┬────┘
   │                            │                              │
   │  POST /api/auth/device     │                              │
   │───────────────────────────►│  生成 device_code            │
   │  {device_code,             │                              │
   │   verification_url,        │                              │
   │   user_code,               │                              │
   │   expires_in,              │                              │
   │   interval}                │                              │
   │◄───────────────────────────│                              │
   │                            │                              │
   │  打开浏览器                │                              │
   │──────────────────────────────────────────────────────────►│
   │                            │    用户访问 verification_url │
   │                            │    （已登录则直接确认）       │
   │                            │◄─────────────────────────────│
   │                            │    未登录则飞书 OAuth 登录    │
   │                            │◄────────────────────────────►│
   │                            │    显示 user_code 确认页     │
   │                            │◄──── 用户点击"授权" ─────────│
   │                            │                              │
   │  GET /api/auth/device/poll │                              │
   │  ?device_code=xxx          │                              │
   │───────────────────────────►│                              │
   │  {session_token, user,     │                              │
   │   team}                    │                              │
   │◄───────────────────────────│                              │
   │                            │                              │
   ✓  登录完成                  │                              │
```

## 接口定义

### 1. 发起设备授权

```
POST /api/auth/device
Content-Type: application/json

Request Body: (可选)
{
  "client_name": "runai",      // CLI 标识，用于审计
  "scope": "team"              // 请求的权限范围
}

Response: 200 OK
{
  "device_code": "d-xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx",
  "user_code": "ABCD-1234",
  "verification_url": "http://dazi.ktvsky.com/app/device?code=ABCD-1234",
  "expires_in": 900,           // 15 分钟过期
  "interval": 5                // CLI 轮询间隔（秒）
}
```

**说明：**
- `device_code`：CLI 用于轮询的密钥，不暴露给用户
- `user_code`：短码，显示在 CLI 和确认页面上，用户核对以防钓鱼
- `verification_url`：用户在浏览器中打开的页面，已包含 user_code 参数
- `expires_in`：device_code 有效期（秒）
- `interval`：建议的轮询间隔（秒）

### 2. CLI 轮询令牌

```
GET /api/auth/device/poll?device_code=d-xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx

Response (用户尚未授权): 200 OK
{
  "status": "pending"
}

Response (用户已授权): 200 OK
{
  "status": "complete",
  "session_token": "xxx.yyy.zzz",
  "user": {
    "id": "user-id",
    "name": "张三",
    "email": "zhangsan@example.com"
  },
  "team": {
    "id": "team-id",
    "name": "My Team"
  }
}

Response (过期): 410 Gone
{
  "status": "expired",
  "error": "device_code has expired"
}

Response (已使用): 409 Conflict
{
  "status": "used",
  "error": "device_code has already been consumed"
}
```

**轮询规则：**
- CLI 按 `interval` 间隔轮询
- 收到 `pending` 继续轮询
- 收到 `complete` 登录成功
- 收到 `expired` 或 `used` 停止轮询

### 3. 前端确认页面

**路径：** `/app/device?code=ABCD-1234`

**页面逻辑：**

```
IF 用户未登录:
    → 先飞书 OAuth 登录，登录后回到本页面

IF 用户已登录:
    → 显示确认页面:
      ┌────────────────────────────┐
      │  CLI 登录请求              │
      │                            │
      │  确认码: ABCD-1234         │
      │  来源: runai               │
      │                            │
      │  [授权]    [拒绝]          │
      └────────────────────────────┘

用户点击 [授权]:
    → POST /api/auth/device/confirm
      {
        "user_code": "ABCD-1234",
        "action": "approve"
      }
    → 页面显示"授权成功，可关闭此页面"
```

### 4. 后端确认接口

```
POST /api/auth/device/confirm
Authorization: Bearer <session_token>  (已登录用户的 session)
Content-Type: application/json

Request Body:
{
  "user_code": "ABCD-1234",
  "action": "approve"          // "approve" 或 "deny"
}

Response: 200 OK
{
  "confirmed": true
}
```

## 后端实现要点

### 数据模型

```sql
CREATE TABLE device_auth_codes (
  id            TEXT PRIMARY KEY,
  device_code   TEXT UNIQUE NOT NULL,
  user_code     TEXT UNIQUE NOT NULL,
  client_name   TEXT DEFAULT 'unknown',
  status        TEXT DEFAULT 'pending',  -- pending | approved | expired | denied
  user_id       TEXT,                     -- 授权后填入
  team_id       TEXT,                     -- 授权后填入
  session_token TEXT,                     -- 授权后生成的新 session
  created_at    TIMESTAMP DEFAULT NOW(),
  expires_at    TIMESTAMP NOT NULL
);
```

### 安全要求

1. **device_code** 用 crypto random 生成，至少 32 字节熵
2. **user_code** 短格式（`XXXX-XXXX`），用大写字母+数字，排除易混淆字符（0/O、1/I/L）
3. **一次性使用**：device_code 被消费后标记为 used，不可重复使用
4. **过期清理**：定期清理超过 `expires_at` 的记录
5. **速率限制**：`POST /api/auth/device` 限制每 IP 每分钟 5 次
6. **轮询限制**：`GET /api/auth/device/poll` 限制每 device_code 每秒 1 次

### 实现复杂度

- 后端：1 张新表 + 4 个端点，约 200 行代码
- 前端：1 个新页面（确认页），约 100 行代码
- 总工作量：约半天

## Runai 端实现

搭子后端就绪后，Runai 的 `sm_dazi_login()` 改为：

```rust
fn sm_dazi_login() {
    // 1. POST /api/auth/device → 获取 device_code + verification_url
    // 2. 打开浏览器到 verification_url
    // 3. 终端显示 user_code，提示用户确认
    // 4. 轮询 /api/auth/device/poll 直到 complete
    // 5. 保存 session_token + team info
    // 完成，用户全程零手动输入
}
```

用户体验：

```
$ runai dazi login

  Please confirm in browser.
  Your code: ABCD-1234

  ⠋ Waiting for authorization...
  ✓ Logged in as 张三, team My Team
```
