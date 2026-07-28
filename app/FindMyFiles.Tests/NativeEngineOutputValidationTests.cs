using FindMyFiles.Engine;
using Xunit;

namespace FindMyFiles.Tests;

public sealed class NativeEngineOutputValidationTests
{
    [Fact]
    public void ValidateBlob_AcceptsBoundedConsistentDescriptor()
    {
        var blob = new NativeEngine.FmfBlob
        {
            Data = new IntPtr(1),
            Len = 7,
            OwnerId = 1,
        };
        var emptyBlob = new NativeEngine.FmfBlob
        {
            OwnerId = 2,
        };

        Assert.Equal(0, NativeEngine.ValidateBlob(emptyBlob));
        Assert.Equal(7, NativeEngine.ValidateBlob(blob));
    }

    [Fact]
    public void ValidateBlob_RejectsMissingOwnerId()
    {
        var blob = new NativeEngine.FmfBlob
        {
            Data = new IntPtr(1),
            Len = 1,
        };

        Assert.Throws<InvalidDataException>(() => NativeEngine.ValidateBlob(blob));
        Assert.Throws<InvalidDataException>(() => NativeEngine.ValidateBlob(default));
    }

    [Fact]
    public void ValidateBlob_RejectsPointerLengthDisagreement()
    {
        var missingData = new NativeEngine.FmfBlob
        {
            Data = IntPtr.Zero,
            Len = 1,
            OwnerId = 1,
        };
        var unexpectedData = new NativeEngine.FmfBlob
        {
            Data = new IntPtr(1),
            Len = 0,
            OwnerId = 2,
        };

        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidateBlob(missingData));
        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidateBlob(unexpectedData));
    }

    [Fact]
    public void ValidateBlob_RejectsPayloadAboveContractCap()
    {
        var blob = new NativeEngine.FmfBlob
        {
            Data = new IntPtr(1),
            Len = EngineContract.MaxPayloadLen + 1,
            OwnerId = 1,
        };

        Assert.Throws<InvalidDataException>(() => NativeEngine.ValidateBlob(blob));
    }

    [Fact]
    public void ValidatePage_AcceptsBoundedConsistentDescriptor()
    {
        var page = new NativeEngine.FmfPage
        {
            RowCount = 1,
            Rows = new IntPtr(1),
            Blob = new IntPtr(2),
            BlobLen = 7,
            OwnerId = 1,
        };
        var emptyPage = new NativeEngine.FmfPage
        {
            OwnerId = 2,
        };

        var lengths = NativeEngine.ValidatePage(page, requestedCount: 1);
        var emptyLengths = NativeEngine.ValidatePage(emptyPage, requestedCount: 0);

        Assert.Equal(EngineContract.RowSize, lengths.RowBytes);
        Assert.Equal(7, lengths.BlobBytes);
        Assert.Equal(0, emptyLengths.RowBytes);
        Assert.Equal(0, emptyLengths.BlobBytes);
    }

    [Fact]
    public void ValidatePage_RejectsMissingOwnerId()
    {
        var page = new NativeEngine.FmfPage
        {
            RowCount = 1,
            Rows = new IntPtr(1),
            Blob = new IntPtr(2),
            BlobLen = 1,
        };

        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidatePage(page, requestedCount: 1));
        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidatePage(default, requestedCount: 0));
    }

    [Fact]
    public void ValidatePage_RejectsRowsBeyondRequestOrContractCap()
    {
        var beyondRequest = new NativeEngine.FmfPage
        {
            RowCount = 2,
            Rows = new IntPtr(1),
            Blob = new IntPtr(2),
            BlobLen = 1,
            OwnerId = 1,
        };
        var beyondCap = new NativeEngine.FmfPage
        {
            RowCount = (uint)EngineContract.MaxPageRows + 1,
            Rows = new IntPtr(1),
            Blob = new IntPtr(2),
            BlobLen = 1,
            OwnerId = 2,
        };
        var empty = new NativeEngine.FmfPage
        {
            OwnerId = 3,
        };

        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidatePage(beyondRequest, requestedCount: 1));
        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidatePage(
                beyondCap,
                requestedCount: (uint)EngineContract.MaxPageRows));
        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidatePage(
                empty,
                requestedCount: (uint)EngineContract.MaxPageRows + 1));
    }

    [Fact]
    public void ValidatePage_RejectsPointerLengthDisagreement()
    {
        var missingRows = new NativeEngine.FmfPage
        {
            RowCount = 1,
            Rows = IntPtr.Zero,
            Blob = new IntPtr(2),
            BlobLen = 1,
            OwnerId = 1,
        };
        var unexpectedRows = new NativeEngine.FmfPage
        {
            RowCount = 0,
            Rows = new IntPtr(1),
            Blob = IntPtr.Zero,
            BlobLen = 0,
            OwnerId = 2,
        };
        var missingBlob = new NativeEngine.FmfPage
        {
            RowCount = 1,
            Rows = new IntPtr(1),
            Blob = IntPtr.Zero,
            BlobLen = 1,
            OwnerId = 3,
        };
        var unexpectedBlob = new NativeEngine.FmfPage
        {
            RowCount = 0,
            Rows = IntPtr.Zero,
            Blob = new IntPtr(2),
            BlobLen = 0,
            OwnerId = 4,
        };

        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidatePage(missingRows, requestedCount: 1));
        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidatePage(unexpectedRows, requestedCount: 0));
        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidatePage(missingBlob, requestedCount: 1));
        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidatePage(unexpectedBlob, requestedCount: 0));
    }

    [Fact]
    public void ValidatePage_RejectsAggregatePayloadAboveContractCap()
    {
        var blobLen = EngineContract.MaxPayloadLen
            - 8
            - (uint)EngineContract.RowSize
            + 1;
        var page = new NativeEngine.FmfPage
        {
            RowCount = 1,
            Rows = new IntPtr(1),
            Blob = new IntPtr(2),
            BlobLen = blobLen,
            OwnerId = 1,
        };

        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidatePage(page, requestedCount: 1));
    }

    [Fact]
    public void ValidateVolumeCount_RejectsNativeCountBeyondSuppliedBuffer()
    {
        var capacity = (uint)EngineContract.MaxVolumes;

        Assert.Equal(
            EngineContract.MaxVolumes,
            NativeEngine.ValidateVolumeCount(capacity, capacity));
        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidateVolumeCount(capacity + 1, capacity));
        Assert.Throws<ArgumentOutOfRangeException>(
            () => NativeEngine.ValidateVolumeCount(0, capacity + 1));
    }

    [Fact]
    public void ValidateResultCount_RejectsUnsignedWraparound()
    {
        Assert.Equal(
            long.MaxValue,
            NativeEngine.ValidateResultCount((ulong)long.MaxValue));
        Assert.Throws<InvalidDataException>(
            () => NativeEngine.ValidateResultCount((ulong)long.MaxValue + 1));
    }
}
