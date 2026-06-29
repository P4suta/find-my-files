using FindMyFiles.Engine;

namespace FindMyFiles.Services;

/// <summary>
/// In-process "soft restart": re-resolve the engine transport and rebuild the
/// page graph <em>without</em> spawning a new process (ADR-0036). This is how a
/// freshly registered service, an applied scope change, or an uninstall takes
/// effect now that the engine is chosen once at startup — the old design
/// relaunched the process, which collides with single-instancing (a relaunched
/// instance redirects its activation back to the still-alive original and exits,
/// dropping the <c>--engine=pipe</c> argument and taking the whole app down).
/// <para>Keeping it in-process also preserves the tray icon and window and skips
/// the WinUI/.NET cold start that ADR-0030's tray-resident mode exists to avoid.</para>
/// <para>Pure over its boundaries (resolve / get-set engine / re-navigate / close
/// diagnostics) so the ordering and disposal contract is unit-tested without a
/// real Frame, engine, or window; <see cref="App"/> wires the production
/// delegates.</para>
/// </summary>
internal sealed class AppReload
{
    private readonly Func<string[], IEngineClient> _resolve;
    private readonly Func<IEngineClient> _getEngine;
    private readonly Action<IEngineClient> _setEngine;
    private readonly Action _renavigate;
    private readonly Action _closeDiagnostics;

    /// <summary>Guards against a re-navigation (or a delegate it triggers)
    /// re-entering <see cref="Run"/> mid-cycle. Reset once the cycle completes,
    /// so a later, independent soft restart still runs.</summary>
    private bool _running;

    /// <summary>Builds the orchestrator over its boundaries; <see cref="App"/>
    /// wires the production delegates once the window exists.</summary>
    /// <param name="resolve">Resolve the engine for the given args (the same
    /// resolve-or-fallback the launch path uses).</param>
    /// <param name="getEngine">Read the current engine (to dispose after the swap).</param>
    /// <param name="setEngine">Publish the freshly resolved engine before the page rebuild.</param>
    /// <param name="renavigate">Rebuild the page graph against the new engine
    /// (production: re-navigate the root Frame to a fresh MainPage).</param>
    /// <param name="closeDiagnostics">Close the diagnostics window — it holds the
    /// old page's view model and polls the old engine, so it must go first.</param>
    internal AppReload(
        Func<string[], IEngineClient> resolve,
        Func<IEngineClient> getEngine,
        Action<IEngineClient> setEngine,
        Action renavigate,
        Action closeDiagnostics)
    {
        _resolve = resolve;
        _getEngine = getEngine;
        _setEngine = setEngine;
        _renavigate = renavigate;
        _closeDiagnostics = closeDiagnostics;
    }

    /// <summary>Run one soft restart: close diagnostics → resolve the new engine →
    /// publish it → rebuild the page → dispose the old engine. Ordering matters —
    /// the new engine is published <em>before</em> the rebuild so the fresh page's
    /// view model binds to it, and the old engine is disposed <em>after</em> the
    /// rebuild so no in-flight handler races a half-torn-down engine (a disposed
    /// engine raises no events, and the old view model unsubscribes itself on its
    /// own Unloaded). Re-entrant calls are ignored.</summary>
    /// <param name="engineArgs">Command-line args steering the engine resolution
    /// (e.g. <c>--engine=pipe</c> after a service register).</param>
    internal void Run(string[] engineArgs)
    {
        if (_running)
        {
            return;
        }

        _running = true;
        try
        {
            _closeDiagnostics();
            var old = _getEngine();
            _setEngine(_resolve(engineArgs));
            _renavigate();
            old?.Dispose();
        }
        finally
        {
            _running = false;
        }
    }
}
