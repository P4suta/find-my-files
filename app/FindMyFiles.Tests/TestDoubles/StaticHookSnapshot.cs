using System.Reflection;

namespace FindMyFiles.Tests.TestDoubles;

internal sealed class StaticHookSnapshot<THooks> : IDisposable
    where THooks : class
{
    private readonly FieldInfo _field;
    private THooks? _original;

    internal StaticHookSnapshot(Type owner)
    {
        ArgumentNullException.ThrowIfNull(owner);
        _field = owner.GetField("_hooks", BindingFlags.Static | BindingFlags.NonPublic)
            ?? throw new InvalidOperationException($"Missing hook field on {owner.FullName}.");
        if (_field.FieldType != typeof(THooks)
            || _field.GetValue(null) is not THooks original)
        {
            throw new InvalidOperationException($"Invalid hook field on {owner.FullName}.");
        }

        _original = original;
    }

    public void Dispose()
    {
        var original = Interlocked.Exchange(ref _original, null);
        if (original is not null)
        {
            _field.SetValue(null, original);
        }

        GC.SuppressFinalize(this);
    }
}
