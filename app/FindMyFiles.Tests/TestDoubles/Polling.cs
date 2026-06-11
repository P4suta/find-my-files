namespace FindMyFiles.Tests.TestDoubles;

/// <summary>Shared polling wait for tests that observe background threads
/// (pipe supervisor, child processes). Import via `using static`.</summary>
public static class Polling
{
    public static async Task WaitUntilAsync(
        Func<bool> condition, string what, int timeoutMs = 5000)
    {
        var deadline = Environment.TickCount64 + timeoutMs;
        while (!condition())
        {
            if (Environment.TickCount64 > deadline)
            {
                throw new TimeoutException($"timed out waiting for: {what}");
            }
            await Task.Delay(10);
        }
    }
}
