using FluentAssertions;
using VivoKsu.App.Services;

namespace VivoKsu.App.Tests;

public class EmbeddedWipeDataTests
{
    [Fact]
    public async Task WriteToAsync_extracts_the_embedded_wipe_data_image_to_disk()
    {
        var directory = Path.Combine(Path.GetTempPath(), "VivoKsu.Tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            var destination = Path.Combine(directory, "wipe-data.img");

            var bytes = await EmbeddedWipeData.WriteToAsync(destination, CancellationToken.None);

            bytes.Should().Be(524288);
            new FileInfo(destination).Length.Should().Be(524288);
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }
}
