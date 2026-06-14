# サプライチェーンと来歴(provenance)

配布物が「このリポジトリの、この commit から、改ざんされていない CI で作られた」ことを
利用者が機械検証できるようにするための仕組みと、その確認手順。コード署名(Authenticode)は
[SIGNING.md](SIGNING.md) を参照。本節は **ビルド来歴(SLSA provenance)・SBOM・依存ピン留め** を扱う。

## 利用者向け: ダウンロードを検証する

`release.yml`(タグ駆動)は GitHub ネイティブの keyless attestation を発行する。**秘密鍵は無く**、
ワークフローの OIDC トークンで Sigstore(Fulcio/Rekor)に署名する。検証に必要なのは `gh` だけ:

```
# ビルド来歴(どの commit / workflow / runner が作ったか)を検証
gh attestation verify find-my-files-vX.Y.Z-win-x64.zip --repo P4suta/find-my-files

# SBOM が同じ zip に紐づくことを検証(CycloneDX predicate)
gh attestation verify find-my-files-vX.Y.Z-win-x64.zip --repo P4suta/find-my-files \
  --predicate-type https://cyclonedx.org/bom
```

検証成功は「成果物のダイジェストが、`P4suta/find-my-files` の `release.yml` から発行された
attestation と一致する」ことを意味する。リリースには以下が添付される:

| 資産 | 内容 |
|---|---|
| `find-my-files-vX.Y.Z-win-x64.zip` | アプリ + エンジンバイナリ(署名有効時は Authenticode 署名済み) |
| `SHA256SUMS.txt` | zip の SHA-256 |
| `fmf-engine.cdx.json` | Rust エンジンの SBOM(CycloneDX 1.6。`cargo-sbom`、ワークスペース全依存) |
| `app.cdx.json` | C# アプリの SBOM(CycloneDX 1.6。CycloneDX dotnet tool、NuGet グラフ) |

zip・SHA256SUMS にはビルド来歴 attestation、各 SBOM には SBOM attestation が紐づく
(リポジトリの **Attestations** タブで一覧。計3件)。

## 依存とビルドの統制

| 面 | 仕組み |
|---|---|
| Rust 依存ロック | `engine/Cargo.lock` / `xtask/Cargo.lock`(commit 済み) |
| C# 依存ロック | `app/FindMyFiles/packages.lock.json` / `app/FindMyFiles.Tests/packages.lock.json`。CI は `-p:RestoreLockedMode=true` で stale を失敗扱い |
| 脆弱性 | `cargo-audit`(RustSec・週次 + lock 変更時)。C# は CodeQL + Dependabot |
| ライセンス/出所 | `cargo-deny`(bans / licenses / sources。未知レジストリ・git は deny) |
| 自動更新 | Dependabot(cargo / nuget×2 / github-actions。週次) |
| Action ピン | 全 workflow の third-party action を **40桁コミット SHA** にピン(`# vX.Y.Z` 併記)。Dependabot が SHA とコメントを更新。`actionlint` が hygiene ジョブで workflow を検証 |
| 姿勢監視 | OpenSSF Scorecard(週次・SARIF を Security タブへ・README バッジ) |
| 再現ビルド | C# は CI で `ContinuousIntegrationBuild=true`(埋め込みソースパス正規化。`Deterministic` は SDK 既定)。Rust は既定で決定論的 |

## メンテナ向け: 初回 attested リリースの runbook

attestation/SBOM ステップはタグ時のみ発火するため、**実タグの前に dry-run** で OIDC/権限経路を確認する:

1. 既存のテストタグ(または使い捨てタグ)で `release` を **`workflow_dispatch`**(入力 `tag_name`)から手動実行。
   `permissions: id-token: write / attestations: write` と各ステップが通ることを確認。
2. 本番は通常どおり `just release`(版 bump + タグ push)→ `release.yml` が自動発火。
3. 完了後、`gh attestation verify <zip> --repo P4suta/find-my-files` が成功し、**Attestations** タブに3件
   (provenance + SBOM×2)、リリースに zip / SHA256SUMS / `*.cdx.json` が揃うことを確認。

### 留意点

- **SBOM ツールは CI/release 限定**で導入する(`mise.toml` の開発ループには入れない)。Rust は
  `cargo install cargo-sbom`、C# は `dotnet tool install --global CycloneDX`(いずれも版ピン)。両言語 **CycloneDX 1.6** に統一。
- ロックファイル更新は Dependabot の nuget PR が `packages.lock.json` を再生成する。ローカルで版を
  足したら `dotnet restore`(両 csproj)→ commit。`Roslynator.Analyzers` の浮動 `4.*` はロックファイルが
  解決版にピンするので、bump 時はロックファイル再生成が要る(= 意図した決定論)。
- 任意の将来拡張: `Microsoft.SourceLink.GitHub`(PDB を commit にトレース可能化)。依存面を増やす分
  見送り中。配布 PDB のデバッグ需要が出たら追加を検討。
