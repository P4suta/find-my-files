using Windows.Storage.Pickers;
using WinRT.Interop;

namespace FindMyFiles.Services;

/// <summary>
/// Folder picker for scope mode (ADR-0024). Unpackaged WinUI 3 requires a
/// picker to be associated with the app window's HWND before it is shown — it
/// throws otherwise — so this always initializes with <see cref="App.WindowHandle"/>.
/// </summary>
public static class ScopeFolderPicker
{
    /// <summary>Show the folder picker and return the chosen absolute path
    /// (<see langword="null"/> when the user cancels). <see cref="FolderPicker"/> has no
    /// multi-select, so the caller invokes this once per folder. Must be called
    /// on the UI thread.</summary>
    /// <returns>The selected folder path, or <see langword="null"/> on cancel.</returns>
    public static async Task<string?> PickAsync()
    {
        var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.Desktop };
        // FolderPicker throws when shown unless at least one filter is present.
        picker.FileTypeFilter.Add("*");
        InitializeWithWindow.Initialize(picker, App.WindowHandle);
        var folder = await picker.PickSingleFolderAsync();
        return folder?.Path;
    }
}
