# ADR-0022: OS/シェル/UI 境界はテスト可能シーム + 挙動テストを必須とする

日付: 2026-06-15 / 状態: 採用済み

## 決定

OS・シェル・プロセス・ファイル I/O・UI イベントに触れるコードは、**注入可能なシーム**(interface、もしくはパス/依存を引数化した `internal` コア)を経由し、その**挙動**を `dotnet test` / `cargo test` で検証するテストを伴わなければならない。純粋ヘルパや引数構築だけをテストして「実挙動は未検証」のまま出荷しない。

正本パターン: `app/FindMyFiles/Engine/IEngineClient.cs`(Fake/Ffi/Pipe)、`Services/IDispatcher.cs`、`Services/IProcessRunner.cs` / `Services/IRevealApi.cs`、`Services/FileLog.cs` のパス引数化コア。エンジン側は `engine/crates/fmf-core/.../seams.rs`(SnapshotStore / JournalSource。シーム2本上限は ADR-0018)。

## 根拠

- **「フォルダーを開いてファイルを選択」(reveal)が初日から壊れていた**: `ShellOps.Reveal` の実挙動(`SHOpenFolderAndSelectItems`)が一度もテストされず、純粋ヘルパ `BuildOpenStartInfo` だけが緑で、CI も通り続けた。テストが品質を保証していなかった。
- 根本原因の型: ランタイム/OS 境界が `static` + 直 P/Invoke のままだと挙動を fake で差し替えられず、挙動検証を書けない。引数/構造のテストは「通る = 壊れていない」を成立させない。
- C# カバレッジゲートが `Threshold=15`(有名無実)だったことも未検証コードの出荷を許した。

## 影響

- 新規の境界コードはレビューで「シーム + 挙動テスト」を要求する(構築だけのテストは不十分とみなす)。
- C# の live UI 自動化は PowerShell スクリプト(`ui-tests.ps1`)前提で、このマシンは実行ポリシーで無効・運用方針でも不採用。よって **UI 隣接ロジックは ViewModel / コアに寄せて `dotnet test` で検証**する(live UI 自動化に依存しない)。
- 形骸テスト(壊しても通る)検出に mutation testing を用いる: Rust = `just mutants`(cargo-mutants)、C# = `just stryker`(Stryker.NET)。当面は情報、段階的にゲート化。
- C# カバレッジゲートは 15% から段階的に引き上げる(ratchet)。

## 再検討トリガ

- live UI 自動化が PowerShell 非依存(例: FlaUI を `dotnet test` に統合)で導入可能になった場合 → UI フローの直接テストを再評価。
- シームの増殖が設計を歪める兆候(エンジン側はシーム2本上限 = ADR-0018 を維持)。
