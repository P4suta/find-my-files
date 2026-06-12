# ADR-0014: ビルドツーリングの却下記録とcodegen-units=1

日付: 2026-06-11 / 状態: 却下を記録(codegen-units=1は採用済み)

## 決定

rust-lld・sccache・cargo-nextestは導入しない。releaseプロファイルは `codegen-units = 1` + `lto = "thin"` を維持する(engine/Cargo.toml)。

## 根拠

- rust-lld vs MSVC link.exe の公平A/B(engineワークスペース3クレート): fmf-cliインクリメンタル 1.72s vs 1.73s、fmf-core変更後の全テストリンク 3.44s vs 3.46s — 差なし。計測ゼロ改善に非標準リンカのリスク(DLL出力・CI差異)は見合わない
- sccacheはincrementalコンパイルを無効化するため却下。cargo-nextestはテストスイートが小さく効果なしのため却下(いずれも同日のA/B判断)
- codegen-units=1: rustcはモジュール単位でcodegen unitを分割するため、クエリカーネルのexec/sweep/matchers/memo分割がインライン喪失で **~10%のクエリレイテンシ** を生む(同一マシン状態でのA/B実測)。1ユニットならホットパスのインラインがモジュール配置から独立する

## 影響

- releaseビルド時間はcodegen-units=1の分だけ延びる(許容)
- クエリカーネルのファイル分割リファクタリングを実行性能と独立に行える
- ビルド高速化の提案はまずこのADRを確認する(再提案防止)
- **rust-cache(Swatinem/rust-cache = GitHub Actions cache)は本ADRの却下対象外**: sccacheと違いrustc呼び出しをラップせず、`~/.cargo`とtargetを成果物としてアーカイブ/復元するだけでincrementalコンパイルを破壊しない。CIの `CARGO_INCREMENTAL=0` もCI workflow限定でローカルのincrementalに非波及。CI高速化(並列job分割・shared-keyキャッシュ共有・dll artifact共有・PR cancel-in-progress)はこれらに該当し許容(ci.yml)

## 再検討トリガ

- ワークスペースが育ちリンクが数十秒級になった場合のみrust-lldを再計測
