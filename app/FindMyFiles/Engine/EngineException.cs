namespace FindMyFiles.Engine;

/// <summary>エンジンが構造化エラーコード付きで操作を拒否した(transport は
/// 生きているがエンジンが失敗を返したケース)。コードは
/// docs/ARCHITECTURE.md の `FMF_E_*` 表に対応する。</summary>
/// <param name="message">エンジンが返した人間可読なメッセージ。</param>
/// <param name="code">`FMF_E_*` 数値コード(<see cref="Code"/> に保持)。</param>
public sealed class EngineException(string message, int code) : Exception(message)
{
    /// <summary>エンジンが返した `FMF_E_*` コード。UI 側の分岐
    /// (例: `FMF_E_LOCKED` で setup 画面へ)に使う。</summary>
    public int Code { get; } = code;
}
