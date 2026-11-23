# Celoom

轻量、封闭的 Linux 原生本地 AI Agent（细胞绽放）。Rust 单进程内嵌 Web 界面，支持浏览器与移动端远程控制，专注 Responses 协议，内置多引擎深度网络搜索、模型质量与网络跳数检测及模型品牌头像识别。

## 构建与启动

依赖：Linux、Rust（1.88+）、Node.js 22+ 与 npm（仅构建前端时需要）。

```bash
cd web && npm ci && npm run build && cd ..
cargo build --release --locked
./target/release/celoom --workspace /path/to/your/project
# 或使用命令 celoom / cel 启动
```

启动后访问 `http://127.0.0.1:4141`（首次访问需设置不少于 12 字节的本机密码）。

常用启动参数：

```text
--bind IP             监听地址，默认 127.0.0.1
--port PORT           监听端口，默认 4141
--workspace PATH      默认工作目录
--data-dir PATH       数据目录，默认 ~/.local/share/celoom
--public-url URL      远程访问的 HTTPS 地址
```

## 作者与贡献者

- 作者：Friendships666
- 贡献者：Antigravity
