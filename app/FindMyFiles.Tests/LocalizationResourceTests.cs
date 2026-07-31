using System.Xml.Linq;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class LocalizationResourceTests
{
    private static readonly string[] ResourceFiles =
    [
        "Strings_en-US.resw",
        "Strings_ja-JP.resw",
        "Strings_zh-Hans.resw",
    ];

    [Fact]
    public void Every_locale_has_the_same_complete_resource_contract()
    {
        var resources = ResourceFiles.ToDictionary(
            file => file,
            LoadResources,
            StringComparer.Ordinal);
        var canonicalKeys = resources[ResourceFiles[0]].Keys.Order(StringComparer.Ordinal).ToArray();

        foreach (var (file, values) in resources)
        {
            Assert.Equal(canonicalKeys, values.Keys.Order(StringComparer.Ordinal));
            Assert.DoesNotContain(
                values,
                pair => string.IsNullOrWhiteSpace(pair.Value));
        }
    }

    [Theory]
    [InlineData("SetupRecovery.Content")]
    [InlineData("DiagCard.Description")]
    [InlineData("ServiceCard.Description")]
    [InlineData("Svc_IdentityUnavailable")]
    [InlineData("Svc_UserDataPurgeFailed")]
    [InlineData("Status_ModePrivileged")]
    [InlineData("StatusVolumeScope.Text")]
    [InlineData("VersionMismatch_RepairAction")]
    public void Recovery_surface_copy_is_localized_in_every_locale(string key)
    {
        foreach (var file in ResourceFiles)
        {
            Assert.True(
                LoadResources(file).TryGetValue(key, out var value)
                && !string.IsNullOrWhiteSpace(value),
                $"{file} is missing a non-empty {key} resource.");
        }
    }

    [Fact]
    public void Supported_volume_copy_names_ntfs_and_every_excluded_volume_family()
    {
        foreach (var file in ResourceFiles)
        {
            var resources = LoadResources(file);
            var copy = resources["Status_ModePrivileged"]
                + resources["StatusVolumeScope.Text"];
            var (removable, network) = file switch
            {
                "Strings_en-US.resw" => ("removable", "network"),
                "Strings_ja-JP.resw" => ("リムーバブル", "ネットワーク"),
                "Strings_zh-Hans.resw" => ("可移动", "网络"),
                _ => throw new InvalidOperationException($"Unreviewed locale: {file}"),
            };
            Assert.Contains("NTFS", copy, StringComparison.Ordinal);
            Assert.Contains("ReFS", copy, StringComparison.Ordinal);
            Assert.Contains("FAT/exFAT", copy, StringComparison.Ordinal);
            Assert.Contains(removable, copy, StringComparison.Ordinal);
            Assert.Contains(network, copy, StringComparison.Ordinal);
        }
    }

    [Fact]
    public void Service_failure_copy_needs_only_the_exit_code()
    {
        foreach (var file in ResourceFiles)
        {
            var value = LoadResources(file)["Svc_Failed"];
            Assert.Contains("{0}", value, StringComparison.Ordinal);
            Assert.DoesNotContain("{1}", value, StringComparison.Ordinal);
        }
    }

    private static Dictionary<string, string> LoadResources(string file)
    {
        var path = Path.Combine(AppContext.BaseDirectory, file);
        var entries = XDocument.Load(path).Root!
            .Elements("data")
            .Select(element => (
                Key: (string)element.Attribute("name")!,
                Value: element.Element("value")?.Value ?? string.Empty))
            .ToArray();
        var resources = entries.ToDictionary(
            pair => pair.Key,
            pair => pair.Value,
            StringComparer.Ordinal);

        Assert.Equal(entries.Length, resources.Count);
        return resources;
    }
}
