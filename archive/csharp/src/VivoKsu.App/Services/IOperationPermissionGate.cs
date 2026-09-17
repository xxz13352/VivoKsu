using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

/// <summary>
/// 用户操作前的服务端许可门禁。每个 OperationCoordinator 操作运行前询问一次;
/// 服务端默认放行,账号被封禁/停用时拒绝。生产由 <see cref="ServerOperationGate"/> 实现,测试可注入 stub。
/// </summary>
public interface IOperationPermissionGate
{
    Task<OperationAuthorization> AuthorizeAsync(OperationKind kind, string title, CancellationToken cancellationToken);
}
