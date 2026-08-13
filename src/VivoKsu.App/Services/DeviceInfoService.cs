using System.Globalization;
using VivoKsu.App.Models;

namespace VivoKsu.App.Services;

public sealed class DeviceInfoService
{
    private readonly FastbootRsBackend backend;

    public DeviceInfoService(FastbootRsBackend backend)
    {
        this.backend = backend;
    }

    public async Task<DeviceDetailsSnapshot> ReadAdbAsync(string serial, CancellationToken cancellationToken)
    {
        var properties = ParseGetProp(await backend.ShellAsync(serial, "getprop", cancellationToken));
        var kernel = await backend.ShellAsync(serial, "uname -r", cancellationToken);
        var slot = await backend.ShellAsync(serial, "getprop ro.boot.slot_suffix", cancellationToken);
        var verifiedBoot = await backend.ShellAsync(serial, "getprop ro.boot.verifiedbootstate", cancellationToken);
        var buildSeconds = await backend.ShellAsync(serial, "getprop ro.build.date.utc", cancellationToken);

        return new DeviceDetailsSnapshot(
            Get(properties, "ro.product.brand"),
            Get(properties, "ro.product.model"),
            Get(properties, "ro.product.device"),
            serial,
            Get(properties, "ro.build.version.release"),
            Get(properties, "ro.build.display.id"),
            ValueOrUnavailable(kernel),
            NormalizeSlot(slot),
            ParseBootloaderState(Get(properties, "ro.boot.flash.locked")),
            ValueOrUnavailable(verifiedBoot),
            "authorized",
            ParseBuildTime(buildSeconds));
    }

    public async Task<DeviceDetailsSnapshot> ReadFastbootAsync(DeviceDetailsSnapshot details, CancellationToken cancellationToken)
    {
        // fastbootd may refuse an unsupported getvar with a non-zero native result;
        // an optional variable must degrade to "Not available" rather than making
        // the whole device-info refresh throw and prevent the device from registering.
        var activeSlot = await TryGetVarAsync(details.Serial, "current-slot", cancellationToken);
        var unlocked = await TryGetVarAsync(details.Serial, "unlocked", cancellationToken);
        var product = ValueOrUnavailable(await TryGetVarAsync(details.Serial, "product", cancellationToken));

        return details with
        {
            Model = IsUnavailable(details.Model) ? product : details.Model,
            Codename = IsUnavailable(details.Codename) ? product : details.Codename,
            ActiveSlot = NormalizeSlot(activeSlot),
            BootloaderState = ParseBootloaderState(unlocked)
        };
    }

    private async Task<string> TryGetVarAsync(string serial, string variable, CancellationToken cancellationToken)
    {
        try
        {
            return await backend.GetVarAsync(serial, variable, cancellationToken);
        }
        catch (FastbootRsNativeException)
        {
            return string.Empty;
        }
    }

    private static Dictionary<string, string> ParseGetProp(string output)
    {
        var result = new Dictionary<string, string>(StringComparer.Ordinal);

        foreach (var line in output.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            var separator = line.IndexOf("]: [", StringComparison.Ordinal);
            if (!line.StartsWith("[", StringComparison.Ordinal) || separator < 1 || !line.EndsWith("]", StringComparison.Ordinal))
            {
                continue;
            }

            result[line[1..separator]] = line[(separator + 4)..^1];
        }

        return result;
    }

    private static string Get(IReadOnlyDictionary<string, string> properties, string key) =>
        properties.TryGetValue(key, out var value) ? ValueOrUnavailable(value) : "Not available";

    private static string NormalizeSlot(string value)
    {
        var normalized = value.Trim().TrimStart('_');
        return string.IsNullOrWhiteSpace(normalized) ? "Not available" : normalized;
    }

    private static string ParseBootloaderState(string value) =>
        value.Trim().Equals("0", StringComparison.OrdinalIgnoreCase) ||
        value.Trim().Equals("yes", StringComparison.OrdinalIgnoreCase) ||
        value.Trim().Equals("true", StringComparison.OrdinalIgnoreCase)
            ? "unlocked"
            : value.Trim().Equals("1", StringComparison.OrdinalIgnoreCase) ||
              value.Trim().Equals("no", StringComparison.OrdinalIgnoreCase) ||
              value.Trim().Equals("false", StringComparison.OrdinalIgnoreCase)
                ? "locked"
                : "Not available";

    private static string ParseBuildTime(string value)
    {
        // DateTimeOffset.FromUnixTimeSeconds throws outside this range; guard against
        // device-controlled ro.build.date.utc values that parse as a long but exceed it.
        const long MinUnixSeconds = -62_135_596_800;
        const long MaxUnixSeconds = 253_402_300_799;
        return long.TryParse(value.Trim(), NumberStyles.Integer, CultureInfo.InvariantCulture, out var seconds)
               && seconds is >= MinUnixSeconds and <= MaxUnixSeconds
            ? DateTimeOffset.FromUnixTimeSeconds(seconds).ToLocalTime().ToString("yyyy-MM-dd HH:mm")
            : "Not available";
    }

    private static string ValueOrUnavailable(string value) =>
        string.IsNullOrWhiteSpace(value) ? "Not available" : value.Trim();

    private static bool IsUnavailable(string value) =>
        string.IsNullOrWhiteSpace(value) ||
        value is "--" or "Not available" or "未检测到设备";
}
