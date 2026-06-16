namespace FindMyFiles.Engine;

/// <summary>Lifecycle of a volume's index (wire values of fmf-core's
/// <c>VolumeState</c>), reported via <see cref="IEngineClient.VolumeUpdated"/>.</summary>
public enum VolumeState
{
    /// <summary>Initial scan in progress; results are incomplete.</summary>
    Scanning = 0,

    /// <summary>Index is complete and searchable.</summary>
    Ready = 1,

    /// <summary>A rescan is rebuilding an already-ready index (e.g. after a
    /// USN gap); the prior result stays usable.</summary>
    Rescanning = 2,

    /// <summary>Indexing failed (access denied, unsupported filesystem); this
    /// volume contributes no results.</summary>
    Failed = 3,
}
