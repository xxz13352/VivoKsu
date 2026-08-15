namespace VivoKsu.App.Services;

public interface IFastbootRsNativeApi
{
    string ListDevices();
    /// <summary>执行 adb shell 命令。timeoutMilliseconds 供重操作(如 magiskboot 解包/重打包)放宽默认 15s。</summary>
    string Shell(string? serial, string command, int timeoutMilliseconds = 15000);
    string GetVar(string? serial, string variable);
    void Reboot(string? serial, string target);
    void FastbootReboot(string? serial, string? target) => throw new NotSupportedException("当前 native 实现不支持 Fastboot 重启。");
    void SetActive(string? serial, string slot) => throw new NotSupportedException("当前 native 实现不支持切换活动槽位。");
    void Push(string? serial, string localPath, string remotePath);
    long Pull(string? serial, string remotePath, string localPath);
    string Install(string? serial, string apkPath, bool replace);
    void Flash(string? serial, string partition, string imagePath);
    void Erase(string? serial, string partition) => throw new NotSupportedException("当前 native 实现不支持擦除分区。");
    long Fetch(string? serial, string partition, string outputPath) => throw new NotSupportedException("当前 native 实现不支持读取分区。");
}
