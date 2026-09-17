# 会话撤销与即时设备目标设计

**状态：** 按用户已授权的敏感区域修复范围执行。

## 目标

修复会话切换、固件工件和 Quick Flash 的三个敏感状态边界，使旧会话的私有能力不能在新会话复用，且执行命令只使用授权后实时发现的唯一设备。

## 约束

- 不恢复或新增运行时镜像、工件或 OTA 哈希/fingerprint 门禁。
- 不恢复或新增跨步骤手机 serial 相等比较、设备 serial capability 绑定或 serial 变化拒绝。
- serial 只可作为实时发现后构造 ADB/Fastboot 命令的短暂目标。
- 保持现有发布物/受控资源完整性校验、路径验证、staging 所有权和多设备拒绝。
- 会话撤销必须先取得 `OperationCoordinator` 的 idle lease；不得在刷写运行时切换凭据或清理 capability。

## 设计

### 登录切换

`auth_login` 成功取得服务器响应后，必须在覆盖内存 token 前取得 idle lease，复用登出既有的撤销顺序：失效会话 capability、停止旧 `SessionLifecycle`、flush usage，再写入新 token。若存在在途操作，拒绝登录切换且不改变旧 token、生命周期或 capability。

### 固件工件

固件工件属于会话私有 staging。所有退出路径均已在 idle lease 下调用 `AppState::revoke_root_capabilities`；撤销逻辑扩展为清空 `firmware_artifacts`，并仅返回/删除 Rust-owned staging 根。这样旧 artifact ID 在新会话无法建立新的 Quick Flash 确认 capability；不需要把设备 serial 或内容摘要塞入工件。

### Quick Flash 目标

Quick Flash 的执行闭包在 `run_async` 授权后实时枚举设备。Fastboot 计划调用既有 `fastboot devices` 唯一设备选择和 `is-userspace` 检查；ADB Root 计划在闭包中执行当前设备发现并要求 ADB transport。发现结果更新 `DeviceRuntime`，随后才重新构造命令。计划内旧 serial 只是预览字段，绝不参与比较。

## 验收

- 已登录状态的重新登录在 busy 时失败且不改变旧会话；空闲切换撤销旧 capability、停止旧 lifecycle 后才替换 token。
- 登出/强制退出后的 firmware artifact ID 无效，其 owned staging 被删除；新 epoch 不能由旧 ID 创建确认计划。
- Fastboot/ADB Root 执行在授权后用实时唯一设备发现结果；多设备、非 fastbootd 或错误 transport 在命令生成前失败。
- 回归测试不重新引入 runtime hash 或跨步骤 serial 绑定。
