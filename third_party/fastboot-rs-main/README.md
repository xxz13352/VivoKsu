<div align="center">

<!-- 头部动画 -->
<img src="https://capsule-render.vercel.app/api?type=waving&color=gradient&customColorList=24&height=200&section=header&text=Fastboot-RS&fontSize=50&fontColor=fff&animation=fadeIn&fontAlignY=35&desc=🦀%20Rust%20实现的高性能%20Fastboot%20工具&descAlignY=55&descSize=18"/>

<!-- 徽章 -->
[![Rust](https://img.shields.io/badge/Rust-1.70+-orange?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT-blue?style=for-the-badge)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%7C%20Linux-green?style=for-the-badge&logo=windows&logoColor=white)](https://github.com/wumai2580/fastboot-rs)

</div>

---

##  项目简介

>  **Fastboot-RS** 是一个用 Rust 从零实现的 Android Fastboot 刷机工具，完全独立于 Google 官方实现。

<div align="center">

```
┌─────────────────────────────────────────────────────────────┐
│                     Fastboot-RS 架构                         │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐        │
│  │   CLI   │  │  Flash  │  │Partition│  │ Progress│        │
│  └────┬────┘  └────┬────┘  └────┬────┘  └────┬────┘        │
│       │            │            │            │              │
│       └────────────┴─────┬──────┴────────────┘              │
│                          │                                  │
│                   ┌──────┴──────┐                           │
│                   │   Protocol  │                           │
│                   └──────┬──────┘                           │
│                          │                                  │
│       ┌──────────────────┼──────────────────┐               │
│       │                  │                  │               │
│  ┌────┴────┐       ┌─────┴─────┐      ┌─────┴─────┐        │
│  │   USB   │       │    TCP    │      │    UDP    │        │
│  └─────────┘       └───────────┘      └───────────┘        │
└─────────────────────────────────────────────────────────────┘
```

</div>

---

##  特性

<table>
<tr>
<td width="50%">

###  核心功能
-  高性能刷写** - 40+ MB/s 传输速度
-  Sparse 镜像** - 完整支持稀疏镜像解析
-  批量刷写** - flashall 一键刷机
-  进度显示** - 实时速度和进度条

</td>
<td width="50%">

###  支持命令
- `flash` / `erase` - 刷写/擦除分区
- `reboot` - 重启设备
- `getvar` - 获取设备变量
- `oem` - OEM 命令
- `set_active` - 设置活动槽位
- `flashall` - 批量刷写

</td>
</tr>
</table>

---

## 快速开始

### 安装

```bash
# 克隆仓库
git clone https://github.com/wumai2580/fastboot-rs.git
cd fastboot-rs

# 编译
cargo build --release
```

### 使用

```bash
# 查看设备
fastboot devices

# 刷写分区
fastboot flash boot boot.img

# 一键刷机
fastboot flashall -p /path/to/package
```

---

## 📁 项目结构

```
fastboot-rs/
├── 源码/
│   ├── main.rs          # 主入口
│   ├── cli.rs           # 命令行解析
│   ├── error.rs         # 错误处理
│   ├── 传输层/          # Transport Layer
│   │   ├── transport.rs # 传输抽象
│   │   ├── usb.rs       # USB 传输
│   │   ├── tcp.rs       # TCP 传输
│   │   └── udp.rs       # UDP 传输
│   ├── 协议层/          # Protocol Layer
│   │   ├── protocol.rs  # Fastboot 协议
│   │   ├── driver.rs    # 驱动封装
│   │   └── sparse.rs    # Sparse 解析
│   └── 功能层/          # Feature Layer
│       ├── flash.rs     # 刷写功能
│       ├── partition.rs # 分区管理
│       └── progress.rs  # 进度显示
```

---

##  性能对比

<div align="center">

| 工具 | 刷写速度 | Sparse 支持 | 跨平台 |
|:---:|:---:|:---:|:---:|
| **Fastboot-RS** | 🟢 40+ MB/s | ✅ | ✅ |
| Google Fastboot | 🟡 30 MB/s | ✅ | ✅ |

</div>

---

## 🤝 贡献

欢迎提交 Issue 和 Pull Request！

---

<div align="center">

<!-- 底部波浪 -->
<img src="https://capsule-render.vercel.app/api?type=waving&color=gradient&customColorList=24&height=100&section=footer"/>

**Made with ❤️ and 🦀 by [GriefRedd](https://github.com/wumai2580)**

</div>
