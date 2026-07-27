namespace FindMyFiles.Engine;

/// <summary>Strict decoder for the overloaded <c>FmfEvent.Entries</c> field
/// when <see cref="EventKind.EngineError"/> is received.</summary>
internal static class EngineErrorSeverityWire
{
    /// <summary>Decode one canonical engine-error severity.</summary>
    /// <param name="value">Unsigned wire/FFI payload.</param>
    /// <returns>The corresponding generated contract value.</returns>
    /// <exception cref="InvalidDataException">
    /// <paramref name="value"/> is not one of the contract's three severities.
    /// </exception>
    internal static EngineErrorSeverity Decode(ulong value)
    {
        if (!Enum.IsDefined(typeof(EngineErrorSeverity), value))
        {
            throw new InvalidDataException(
                $"engine-error severity {value} is outside the contract");
        }

        return (EngineErrorSeverity)value;
    }
}
