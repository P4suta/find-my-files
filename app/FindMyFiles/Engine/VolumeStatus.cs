namespace FindMyFiles.Engine;

/// <summary>A volume's current index status — the payload of an
/// <see cref="IEngineClient.VolumeUpdated"/> event and of
/// <see cref="IEngineClient.GetStatusAsync"/>.</summary>
/// <param name="Label">Drive label (e.g. <c>"C:"</c>).</param>
/// <param name="State">Where the index is in its lifecycle.</param>
/// <param name="Entries">Indexed entry count so far (grows while
/// <see cref="VolumeState.Scanning"/>).</param>
public sealed record VolumeStatus(string Label, VolumeState State, ulong Entries);
