namespace VivoKsu.App.Models;

public enum OperationKind
{
    Idle,
    Discovering,
    Rebooting,
    Installing,
    Transferring,
    Hashing,
    Flashing,
    Mirroring,
    Completed,
    Canceled,
    Failed
}
