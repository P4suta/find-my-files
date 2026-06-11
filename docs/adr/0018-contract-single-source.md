# ADR-0018: 契約の単一正本化(fmf-contract)+捕獲先行ゴールデンコーパス

日付: 2026-06-11 / 状態: 採用済み(ADR-0016 の「契約定数は複製し値ピンで同期」運用のみ supersede。
named pipe 採用・トランスポート却下案・flush 公開面・配布の決定は不変)

## 決定

エンジン契約(ステータスコード・オペコード・イベント種・ワイヤ POD・QueryOptions・上限値・
版数・pipe名)の機械可読正本として、**依存ゼロの leaf クレート `fmf-contract`(rlib)** を
依存グラフ最下層に新設し、全 Rust 消費者(fmf-core / fmf-proto / fmf-ffi / fmf-service)へ
単一定義を放射する。C# へは `fmf-contract` 内の `gen-contract` バイナリが
`app/FindMyFiles/Engine/Generated/EngineContract.g.cs`(定数・enum・`[StructLayout(Explicit)]`
構造体・CountersData DTO)を**チェックイン生成物**として放射し、`fmf-contract/tests/drift.rs`
(再生成とコミット済み生成物のバイト一致)が `cargo test --workspace` 内で漂流を常時検出する。

契約の意味論はリポジトリルートの **`contract/golden/`**(manifest + バイト列 + 共有 JSON fixture)
が実行可能仕様として担う。コーパスは**リファクタ開始前に現行実装から捕獲**(capture-first)し、
以後 Rust(fmf-proto)と独立手書きの C# codec(PipeProtocol/PageCodec)の両方が同一ファイルを
ピンする。再捕獲(bless)は `FMF_BLESS=1` を付けた明示実行のみ — 意図的な契約変更の儀式であり、
通常実行のテストは既存バイトとの一致を要求する。

あわせて、エンジン内部の OS 効果シームを **`SnapshotStore` / `JournalSource` の 2 trait のみ**に
限定して導入し(volume worker の障害経路を非昇格・決定的テストに降ろすため)、これを上限として
追加のポート化を禁止する。

## 根拠

### 複製の根拠は Cargo の事実誤認だった

現行 `fmf-proto/src/lib.rs:3-5` と `fmf-ffi/Cargo.toml` は「fmf-ffi は cdylib なので依存できない/
されられない、ゆえにエラーコード表は複製し contract_tests の値ピンで同期する」と主張する。
しかし不可能なのは「他クレートが cdylib **に**依存する」方向だけで、**cdylib が rlib に依存する
ことは普通にできる** — fmf-ffi は現に fmf-core(rlib)に依存しており、dev-deps では fmf-proto
にも依存している。依存ゼロの rlib を最下層に置けば、二重定義は「テストで事後検出」ではなく
「定義が1つしかないので漂流しえない」に置き換わる。監査で確定した high 6件のうち3件
(コード表二重定義・イベント種マジックナンバー散在・「同一ゴールデンバイトをピン」主張の未達)
はこの一手で構造的に消える。

### capture-first(コーパスが先、リファクタが後)

ゴールデンコーパスを「新契約クレートから生成」すると、生成器のバグが仕様そのものに焼き付く
(自己整合堕ち: 生成器が自分と一致するだけのテスト)。先に**現行実装のバイト**を捕獲して封印する
ことで、(1) S1 以降の「ワイヤ・ABI バイト不変」が傍証でなくバイト一致で証明され、(2) 生成系は
「捕獲済みバイトを再現できること」を要求される側に回る。

### 生成方式: 明示コマンド+チェックイン+drift テスト(ADR-0014 非該当)

`gen-contract` は MSBuild/ビルドフックに組み込まない(独自 Directory.Build.props 禁止則、
ADR-0014 のビルド複雑化却下と整合)。`just contract-gen` の明示実行+生成物コミット+
`cargo test --workspace` 内の drift 検証(既存 lefthook pre-push / CI の test ジョブに無改造で
乗る)で同等の保証を得る。FieldOffset 等の値は**コンパイル済み Rust 型の `offset_of!` 実値**から
取るため手計算ゼロ・値漂流は型システム上不可能。列挙漏れは drift+golden+C# 側 `Marshal.SizeOf`
起動時アサートの三重で検出する。

### 却下案

- **フル Platform ポート化(〜10 trait + fmf-win 新クレート)**: Windows 専用憲章への投機的一般化。
  全 I/O シームの保守面積が恒久2倍になる。テスト価値が具体的に立証できた 2 シームのみ採用し、
  本 ADR を上限の正本とする
- **ワイヤ版数上げ(pipe 名 v2 / イベント opcode 整理 / PROTOCOL_VERSION=2)**: バイト不変原則と
  矛盾し、捕獲コーパスと perf-gate の「純粋な回帰オラクル」性を毀損する無償スコープ。イベント
  フレームの opcode 二重用途(要求番号と重複、flags で弁別)は footgun だが文書化+テスト済みで、
  変更の便益が儀式コストに見合わない。必要になれば本移行完了後に独立 ADR で
- **契約定義のマクロ DSL(contract_consts! 等)**: 約40定数+6 POD に対して機構過剰。素の定義+
  `meta()` 関数(offset_of! 直読み)で同じ保証が得られる
- **gen 専用の独立クレート**: `fmf-contract/src/bin/gen-contract.rs` で足りる。クレート数を
  6 に抑える
- **fmf-proto → fmf-core 依存(変換層を core 側に置く案)**: 契約正本の leaf 性が Cargo で
  強制されなくなる。依存方向は core→contract に統一し、変換層そのものを消滅させる
- **全面語彙置換(scan→ingest / diag→obs 等)**: ADR 17本の歴史記録の言語と恒久乖離し
  「構造を変える前に該当 ADR を読む」ワークフローを劣化させる。採るのは「叙述順=フロー順」のみで
  命名は不変
- **volume worker の全面状態機械リライト**: checkpoint-after-apply・コンパクション世代再検査など
  散文でしかピンされていない並行性不変条件の書き直しは、新作テストでは新旧等価を証明できない。
  挙動保存の純関数抽出+2シームに限定する

## 影響

- 新規 crate 1つ(fmf-contract)。**DLL 名 `fmf_engine`・pipe 名 `fmf-engine-v1`・ABI_VERSION=1・
  PROTOCOL_VERSION=1・FMFIDX04 はすべてバイト不変**(版数上げなし)
- fmf-ffi の contract_tests は「複製等値ピン」から「リテラル絶対値ピン+ABI レイアウトピン」へ
  昇格して存続 — 単一正本そのものの誤編集を下流テストが割って検出する独立トリップワイヤ
- 契約変更の正規フロー(一方向放射): docs/ARCHITECTURE.md(散文)→ fmf-contract(定義)→
  `FMF_BLESS=1` 再捕獲 → `just contract-gen` → 両言語テスト green。エラーコード表は従来どおり
  追記のみ・再番号禁止
- C# 側決定(ユーザー確定): CountersData も生成対象(カウンタ追加が C# へ自動追従)/
  `ISearchResult.GetRangeAsync` にも CancellationToken を完全伝播(epoch 機構との二重防御を
  挙動テストで固定)
- 移行は11ステージ(S0→S0.5→S1a→S1b→S2 は厳密順序、S3⇔S4、S5a/S5b⇔S4/S4b は並行可)。
  各ステージ単独でコンパイル可+全テスト green で main へマージ可能。fmf-core 接触ステージ
  (S1b/S3/S4/S4b)は昇格シェルで `just perf-gate` green をマージ条件とする

### S4(scan.rs 解体)の退路宣言

scan/ 分割で criterion 10% ゲートを超過した場合は、原因調査より先に**ファイル統合へ即時
ロールバック**し、ADR-0014 の計測手順(基準コミット worktree との同時刻交互 A/B)で再判定する。
`codegen-units=1` のためモジュール境界はインライン化に中立のはずだが、仮説より計測を優先する。

## 検証

- [ ] S0.5: 捕獲コーパスを Rust/C# 両スイートが同一ファイルでピン(非昇格 `cargo test` +
  `just test-app`)
- [ ] S1a: 依存反転後、コーパス一致がワイヤバイト不変を証明。C# 無変更で全テスト通過(二重証明)
- [ ] S2: 生成コーパス==捕獲コーパスのバイト一致(自己整合堕ち封じ)+ drift テスト稼働
- [ ] S4: `streaming_scan_matches_reference`(昇格)+ perf-gate green
- [ ] S4b: worker 障害経路(snapshot 破損→rescan / journal-gone→Rescan→Ready / 保存失敗)が
  非昇格・決定的テストで green、実 C: スモークで新旧挙動同一
- [ ] S6: 昇格シェルで perf-gate + FMF_ADMIN_TESTS + FMF_PIPE_TESTS 一括 green、本付録の
  出発点と数値比較

## 再検討トリガ

- 2 シームで覆えない admin 専用障害経路の実退行が発生した場合(ポート追加はそのとき個別 ADR で)
- 契約変更頻度が上がり bless 儀式が摩擦になった場合(生成系のビルド統合を再評価)
- pipe ページ取得 p99 > 5ms 常態化(ADR-0016 の再検討トリガを引き継ぐ)

## 付録: 旧→新パス対応表(履歴調査用。`git log --follow` の補助)

| 旧 | 新 |
|---|---|
| fmf-proto `codes`/`PIPE_NAME`/`PROTOCOL_VERSION` | fmf-contract `codes`/`versions`(protoは再公開) |
| fmf-proto `QueryOptionsWire`/`WireRow`/`EventWire` | fmf-contract `pod::{FmfQueryOptions, FmfRow, FmfEvent}` |
| fmf-ffi `FMF_*` 定数・POD定義・`volume_bytes` | fmf-contract から再エクスポート/`volume::encode_label` |
| fmf-ffi/`error_chain` ・ fmf-service/dispatch `error_chain` | fmf-core `diag::error_chain`(4KiB上限) |
| fmf-core `engine::VolumePhase` | fmf-contract `options::VolumeState`(名称も統一) |
| fmf-core `scan.rs`(1165行) | `scan/{mod,volume_io,pipeline,parse,deferred,probe}.rs` |
| fmf-core `engine/volume.rs` のスレッド本体 | `engine/worker.rs`(+`seams.rs`+`worker_tests.rs`) |
| fmf-cli `main.rs`(878行) | `main.rs`(135行)+`cmd/{index,stats,bench,io_probe,criterion_gate,diag}.rs`+`bench_support.rs` |
| C# `NativeEngine` 構造体・status定数 | `Engine/Generated/EngineContract.g.cs`(生成・partial NativeEngine) |
| C# `IEngineClient.cs` 内 DTO 群 | `Engine/EngineTypes.cs`(CountersData は生成物へ) |
| C# `PipeEngineClient.cs` 内 接続・結果ハンドル | `Engine/Transport/{PipeConnection,PipeSearchResult}.cs` |
| C# `MainPage.xaml.cs`(452行)の viewport/perf/converter | `Controls/ResultsViewportManager` / `Views/PerfPanel` / `Converters/UiConverters`(残181行) |
| C# `App.xaml.cs` の3種例外ハンドラ | `Services/ExceptionPolicy.cs` |
| 各テストの一意tempdir複製(%TEMP%) | fmf-core `index::testutil::TestDir`(target/test-tmp・RAII) |

実施ステージのコミット: S0=9f7f4a6 / S0.5=c3916df / S1a=c9eb007 / S1b=fdb5407 / S2=7ce58e7 /
S3=6855336 / S4=4e99077 / S4b=261fbb7 / S5a=289e60a / S5b=540d79c / S6a=6226ea8 /
S6b=287f659+9d7a30d(+文書収束コミット)。

## 付録: 出発点記録(refactor 開始時点)

- 基準コミット: `97df250`(= feat/v2-service-split 完了、main へ ff マージ済み)
- 実測値(2026-06-11、ADR-0016 検証節より): 初回インデックス実 C: 2.31s @1,268,560件 /
  USN→イベント 250.9ms / kill→復元 1.25s(restore p50 108ms)/ 検索 p99 ≤5.6ms /
  ループバック ResultPage p99 ≤5ms / RAM ~99B/entry(WS 119.9MiB @1.27M)
- 非昇格ゲート(`just verify`)green 確認: 2026-06-11 ブランチ作成直後に実行 —
  fmt-check / clippy -D warnings / cargo test --workspace / C# 80/80 すべて pass
- 昇格ゲート(`just perf-gate` / `FMF_ADMIN_TESTS=1`): S1b 着手前にユーザー実行で再確認予定
