# ADR-0016: v2サービス分離 — fmf-service + named pipe

日付: 2026-06-11 / 状態: 採用済み

## 決定

エンジンを特権サービス `fmf-service`(fmf-core を直接ホスト、LocalSystem)に載せ、UIは非特権
(asInvoker)化して named pipe で接続する。ワイヤ定義は新規 rlib `fmf-proto` に置き、
`PipeEngineClient` が `IEngineClient` の第3実装になる。仕様の正本は docs/ARCHITECTURE.md
「Pipe プロトコル」節。FFI(fmf_engine.dll)と in-proc 経路は当面存続する(`--engine=inproc`、
要手動昇格)。

## 根拠

- MVPの requireAdministrator はアプリ全体を管理者で走らせる: UIPI で Explorer→ウィンドウの
  drag & drop が死に(README既知制限)、「開く」は explorer.exe 経由の脱昇格ワークアラウンドが必要だった
- 設計は最初からこの分離を予約済み: fmf-ffi 無ロジック則、IEngineClient 差し替え境界、
  %ProgramData% のマシン単位インデックス、エラーコード表の「pipe プロトコルと共用」
- 常駐サービスは「UIが起動していなくてもUSN追従でインデックスが新鮮」を実現する(Everything Service と同形)

### トランスポートの却下案

- **COM / RPC(out-of-process)**: レジストリ登録・マーシャリング定義・昇格境界の複雑さが、
  長さ前置きフレームの named pipe に対して見合わない。ワイヤの可観測性(フレームをそのまま
  ログ/テストにピンできる)も劣る
- **gRPC / HTTP(localhost)**: ネットワークスタック経由は「やらないことリスト」のサーバ機能に接近し、
  依存(tokio/tonic)が fmf-core の同期スレッド文化と衝突する。ローカルIPCに HTTP/2 は過剰
- **共有メモリ+イベント**: ページ転送は最速だが、寿命・権限・世代管理を自前設計することになり
  「FFI 1関数=1メッセージ」の単純な写像が失われる。pipe の往復(基準値は ARCHITECTURE.md
  遅延予算節)で予算に対し余裕があるため不要
- **asyncランタイム(tokio)**: 接続は高々数本。blocking I/O+スレッドが既存設計と整合。
  依存とビルド時間だけ増える

### flush の公開面(3案)

前提として `Engine::flush()` を実体化する(VolumeSlot の共有チェックポイント+世代ペアの dirty-skip)。
公開面は3案を比較した:

- **①pipe オペコードとして公開** — 却下。クライアント起動の flush 連打は index.read() 保持の反復で
  USN 適用を停止させるローカル DoS 経路(SECURITY.md 脅威6)
- **②FFI にも置かずサービス内部関数のみ** — 却下。in-proc(--engine=inproc)経路とテストが
  保存タイミングを再現できず、契約の写像表(FFI 1関数=1メッセージ)にも穴が開く
- **③採用: FFI `fmf_flush` はエクスポート、pipe はオペコード11の番号予約のみ**

保存はサービス内部の責務 — 定期(既定300s・volume間スタガ・dirtyのみ)+SCM Stop/PRESHUTDOWN 時。
PRESHUTDOWN の既定猶予は現行 Windows では 10 秒に短縮されているため(docs/RESEARCH.md)、
install 時に `SERVICE_PRESHUTDOWN_INFO` で明示的に延長を設定する。

### 配布

MSIX/インストーラは本マイルストーンでは見送り(WindowsPackageType=None 維持)。サービス導入は
`fmf-service install`(SID捕捉・DACL設定・特権剥奪を原子的に行うため sc.exe では代替不能)+
justfile レシピ+README 手順で成立させる。**asInvoker への切替はサービス導入手段の成立が前提条件**
(サービス未導入の既定挙動: 説明付き InfoBar+fake フォールバック+「管理者として再起動」ボタン)。

## 影響

- 新規 crate 2つ(fmf-proto / fmf-service)。fmf-ffi と DLL 名 `fmf_engine` は不変更
- IEngineClient の同期3メソッド(ListVolumes/StartIndexing/GetStatus)が Task 返しになる
  (pipe 越え同期=UIスレッド「固まらない」違反)
- 単一書き手不変条件がプロセス間に拡張: `{index_dir}\.writer.lock` + `FMF_E_LOCKED=7`
- Rust/C# 両テストスイートに同一のゴールデンフレーム(バイト列)をピンし、ワイヤの漂流を
  contract_tests と同じ流儀で固定する
- FfiEngineClient(--engine=inproc)の削除トリガ: サービスGA後1リリースのソーク完了
- drag-out(結果→Explorer)は本マイルストーン外の新機能として別起票(drop 方向の解消のみ実装)

## 検証(2026-06-11 実測。数値の正本は CLAUDE.md 性能合格ラインと ARCHITECTURE.md 遅延予算節)

- [x] 初回インデックス 実C: **2.31s @1,268,560件**(ゲート: 100万≈60s。`just bench-check`)。
  サービス実バイナリ経由のE2E(`service_admin.rs`、コンソールモード子プロセス)でも実C:スキャン→
  Ready→クエリ成立を確認
- [x] USN→イベント **250.9ms**(ゲート1s。定期flush 10s間隔が発火する構成下で計測。内訳ほぼ全てが
  意図したエンジン側デバウンス200ms。UI側は既存の50msデバウンス+描画が乗る)
- [x] kill→再起動→復元 **1.25s**(プロセス起動込み。ゲート2s。エンジン単体の restore p50 は 108ms)。
  ハードkill前の定期flushでスナップショットが残ること(耐久性)も同テストで証明
- [x] 検索 p99 **≤5.6ms** 実C: 全クエリ(ゲート50ms)/ ResultPage 64行のループバック往復 p99 **≤5ms**
  をテストが常時アサート(`pipe_loopback.rs::page_roundtrip_stays_inside_the_latency_budget`)
- [x] RAM: エンジンは fmf-cli 計測と同一コード(~99B/entry、WS 119.9MiB @1.27M)。fmf-service の
  追加分は pipe スレッドとキューのみ(イベントキュー上限 256×32B/接続)
- [ ] SCM 登録(`fmf-service install` → start → stop → uninstall)の実機スモーク — **手動手順として残置**。
  永続的な LocalSystem 自動起動サービスの登録はユーザー操作で行う(`just service-install`)。
  SCM 経路のコードは windows-service crate 経由で、serve コアはコンソールE2Eと共通
- [ ] SECURITY.md の手動検証チェックリスト(別ユーザー拒否・リモート拒否は別トークン/別マシンが必要)

## 再検討トリガ

- pipe ページ取得 p99 が実測で 5ms を超える環境が常態化した場合(複数ページ一括取得オペコード、
  または共有メモリページ転送を再評価)
- マルチユーザー同時利用の実需要(`fmf-service authorize <user>` での認可SID複数登録)
