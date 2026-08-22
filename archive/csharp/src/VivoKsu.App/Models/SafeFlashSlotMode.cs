namespace VivoKsu.App.Models;

/// <summary>VIVO 线刷刷写槽位目标(双槽设备)。CurrentSlot 为默认,零额外设备查询。</summary>
public enum SafeFlashSlotMode
{
    /// <summary>刷入当前活动槽(默认;fastboot 自动用当前槽)。</summary>
    CurrentSlot,

    /// <summary>刷入另一侧槽位,刷完后切到该槽启动。</summary>
    OtherSlot,

    /// <summary>两侧槽位都刷入。</summary>
    BothSlots
}
