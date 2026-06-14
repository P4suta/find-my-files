# ADR-0021: ビルド出力の単一 build/ ツリー集約

日付: 2026-06-14 / 状態: 採用

## 決定

全てのビルド成果物をリポジトリルートの単一 `build/` ツリーに集約する。

```
build/
├── engine/        # engine ワークスペースの cargo target-dir
├── xtask/         # xtask ワークスペースの cargo target-dir
├── app/           # C# の bin 出力(FindMyFiles / FindMyFiles.Tests)
├── dist/FindMyFiles/   # publish バンドル
├── package/       # リリース zip + SHA256SUMS.txt
├── sbom/          # CycloneDX SBOM(release.yml)
├── site/          # GitHub Pages 組立(landing + book + doc)
└── docs-book/     # mdBook 出力
```

機構(いずれも禁止規約に抵触しない手段):

- **Rust**: ワークスペース毎に `.cargo/config.toml` の `[build] target-dir`(`engine/.cargo` → `../build/engine`、`xtask/.cargo` → `../build/xtask`)。相対パスは `.cargo/` の親基準で解決される(`cargo metadata` で実測確認済み)。**リポジトリルートに1本置く案は却下**(両ワークスペースが同一 target を共有し ADR-0018 の分離則を壊す)。
- **C# bin**: 各 csproj の `BaseOutputPath`(`..\..\build\app\<proj>\`)。
- **dist/package/site**: `xtask/src/paths.rs` を単一正本に(`build_root`/`dist_dir`/`package_dir`/`engine_release_dir`/`site_dir`)。
- **mdBook**: `docs/book.toml` の `build.build-dir = ../build/docs-book`。

## 根拠

- 成果物が `engine/target`・`xtask/target`・`app/**/bin`・ルート `dist/`・ルート zip・ルート SBOM・`site/` に散在し、把握と掃除のコストが高かった。`build/` 1本なら「消せば全部消える」+ `.gitignore` 実質1行。
- `.cargo/config.toml` の target-dir はツールチェーンピンではないため `rust-toolchain.toml`/`global.json` を置かない規約(mise 二重管理回避)に非抵触。

## 影響

- **C# obj は据え置き**(`app/**/obj/`)。obj 移設は `BaseIntermediateOutputPath` を restore 前評価で効かせる必要があり `Directory.Build.props` が事実上必須だが、CLAUDE.md は同ファイルを禁止(`winapp run` のアナライザ注入を黙って shadow するため)。obj は中間物で gitignore 済みのため実害なし。
- dev ツリーの `fmf-service.exe` 探索(`ServiceSetup.cs` 本番 + pipe/contract テスト)は `build/engine/release` に追従。
- `testutil.rs` の test-tmp フォールバック既定は `build/engine`(config.toml の target-dir は `CARGO_TARGET_DIR` env を立てないため)。
- CI(ci/release/pages)のアーティファクト・SBOM・パッケージ・Pages パスを全て `build/` 配下へ更新。`site/` は committed なランディング源のまま、組立出力は `build/site`。
- 旧来の `engine/target` 等を前提にしたツール(rust-analyzer 等)は config.toml を尊重するため追従(必要ならリロード)。

## 再検討トリガ

- C# obj もルートから消したい要求が強まり、`winapp run` のアナライザ注入機構が `Directory.Build.props` 非依存に変わった場合(props 解禁の是非を再評価)。
