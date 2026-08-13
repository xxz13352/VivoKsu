using CommunityToolkit.Mvvm.ComponentModel;
using VivoKsu.App.Models;

namespace VivoKsu.App.ViewModels;

public sealed partial class QuickFlashPresetItemViewModel(
    QuickFlashPartition partition,
    string displayName) : ObservableObject
{
    public QuickFlashPartition Partition { get; } = partition;

    public string DisplayName { get; } = displayName;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ImagePath))]
    [NotifyPropertyChangedFor(nameof(HasImage))]
    private FlashImageInfo? selectedImage;

    public string ImagePath => SelectedImage?.Path ?? string.Empty;

    public bool HasImage => SelectedImage is not null;
}
