# アーキテクチャと FFI 正本契約

このファイルが **FFI契約の正本**。エンジン(Rust)とUI(C#)の双方はここに従う。シグネチャ変更はまずこのファイルを更新してから両側を直す。

## 全体構成

```
┌────────────────────────────────────────────────────┐
│ WinUI 3 アプリ (C#/.NET, asInvoker)                  │
│   ViewModels ── IEngineClient (差し替え境界)          │
│        ├─ PipeEngineClient (既定: named pipe)         │
│        ├─ FfiEngineClient (--engine=inproc・要昇格)   │
│        └─ FakeEngineClient (--fake-engine)            │
└───────┬──────────────────────────┬─────────────────┘
        │ named pipe               │ C ABI (in-proc)
┌───────▼────────────────────┐ ┌──▼──────────────────┐
│ fmf-service (特権サービス,   │ │ fmf_engine.dll        │
│  LocalSystem・特権最小化)    │ │  (fmf-ffi crate,      │
│  pipeサーバ+SCM+定期flush  │ │   cdylib) 変換・       │
│  ワイヤ定義 = fmf-proto rlib │ │  ハンドル管理・        │
│                            │ │  catch_unwind のみ    │
├────────────────────────────┴─┴─────────────────────┤
│ fmf-core (rlib): VolumeIndex / query /               │
│   mft scan (ntfs-reader) / usn tail / persist        │
└──────────────────────────────────────────────────────┘
```

**FFI 1関数 = pipe 1オペコード、イベントコールバック = pipe プッシュ通知**。ワイヤ仕様は本書
「Pipe プロトコル」節が正本(設計判断は [ADR-0016](adr/0016-service-split-named-pipe.md) /
[ADR-0017](adr/0017-service-security-model.md))。

## モジュールマップ(1ファイル=1責務)

```
fmf-core/src/
├─ index/        mod(型+再エクスポート+in-placeマージ) / core(VolumeIndex+読み取り+派生キャッシュ)
│                / mutate(USNミューテーション) / snapshot(永続化、unsafe POD はここに封じ込め)
│                / builder(2パス構築+EXCLUDED伝播) / compact(コンパクション) / frn / testutil
├─ query/        mod(AST/compile 公開面) / exec(searchドライバループ+materialize)
│                / sweep(pool-sweep候補生成) / matchers(残余評価) / memo(DirPaths/OffsetTable)
├─ engine/       mod(Engine+ライフサイクル+イベント) / volume(VolumeSlot+スレッド防火壁+install_index)
│                / search(ボリューム横断+k-wayマージ) / results(ResultSet+STALE判定) / tests
├─ mft.rs / scan.rs / usn/{records,apply,session} / metrics.rs / diag.rs / wtf8.rs
fmf-ffi/src/     lib(エラーコード+再エクスポート+エクスポートピン) / error / handle / events
                 / volumes / blob / results / contract_tests(ABIレイアウト・null・エラー経路の固定)
fmf-proto/src/   lib(PROTOCOL_VERSION+エラー定数。fmf-ffiのcontract_testsが値をピン)
                 / frame(16Bヘッダ+長さ前置きcodec) / messages(オペコード+wire構造体)
fmf-service/src/ lib(モジュール公開 — ループバックテストが実サーバを駆動) / main(clap: run)
                 / pipe(overlapped I/OのRead/Write化+listener。accept は connect/停止Event の2-wait)
                 / server(接続毎: reader+worker2+書き込みmutex) / dispatch(オペコード→Engine、
                 catch_unwind防火壁、結果ハンドルLRU64) / events(Subscribe+有界キュー256)
                 / config(service.json) / host(ロック敗者の5s→60sリトライ)
                 / faults(--debug-faults: !!lag/!!panic/!!drop)
                 / security(SDDL構築ピン+SID捕捉+接続時トークン照合+dir DACL)
                 / svc(serve共通コア+SCMエントリ: Stop/PRESHUTDOWN→flush→graceful)
                 / main(run/install/uninstall --purge-data/start/stop/status)
app/FindMyFiles/
├─ Engine/       IEngineClient(境界) / FfiEngineClient / PipeEngineClient / FakeEngineClient
│                / NativeEngine(P/Invoke) / PipeProtocol(codec) / PageCodec(FmfRow+blob→RowData)
│                / EngineClientFactory(CLI>settings>auto の選択)
├─ ViewModels/   MainViewModel(合成ルート) / SearchOrchestrator / ResultsPresenter
│                / NotificationCenter / PerfPanelViewModel / StatusFormatter / ResultRow
├─ Virtualization/ VirtualResultList(生涯単一+Reassign/epoch)
├─ Services/     IDispatcher(テストシーム) / DispatcherQueueDispatcher / Notifier / FileLog / ShellOps
│                / AppSettings(%APPDATA%\settings.json: engineモード等。破損はwarn+既定値+.bad退避)
└─ FindMyFiles.Tests/  xUnit(ManualDispatcher fake で決定的にUIスレッド模倣)
```

新フィールド・新メソッドの可視性は「その責務のディレクトリ内」を既定とする(`pub(super)`)。crate 外公開は mod.rs の `pub use` 経由のみ。

## エンジン内部の要点

現在の構造のみ記す。判断理由・実測根拠・却下案は `docs/adr/` 参照。

- **VolumeIndex(ボリューム毎、struct-of-arrays)**: 名前は fold-overflow レイアウト([ADR-0004](adr/0004-fold-overflow-name-layout.md)) — スイープ対象は fold 済み `lower_pool` 1本、原文は差分時のみ `orig_pool`+`orig_off`(`u32::MAX`=fold同一)。fold は長さ保存([ADR-0003](adr/0003-wtf8-length-preserving-fold.md))。size は u32 カラム+オーバーフロー map([ADR-0007](adr/0007-size-u32-overflow.md))。FRN→EntryId はソート済み id 順列、キーは frn カラム間接参照([ADR-0005](adr/0005-frn-index-sorted-permutation.md))。常時維持のソート順列は name のみ、size/mtime 順は遅延 derived([ADR-0006](adr/0006-lazy-sort-permutations.md))。パス文字列は保持せず親チェーンで遅延構築。削除は tombstone、閾値超でコンパクション。
- **USNバッチのソート構造維持**: 挿入位置二分探索+in-place セグメント移動(`index/mod.rs merge_sorted_tail`、[ADR-0008](adr/0008-insertion-point-batch-merge.md))。
- **コンパクション**: volume スレッドがバッチ適用毎に判定(`len≥100k && (tombstone>12.5% || dead_name_bytes>32MiB)`)。旧id昇順リマップで perm/FRN索引は再ソートなし([ADR-0009](adr/0009-compaction-order-preserving-remap.md))。read guard 下でコピー構築→`install_index` で swap+structural bump→開いている結果ハンドルはハードSTALE。死んだ dir の子は root へ(push_raw の orphan 方針)。
- **FRN索引のlookup意味論**: 未マージ尾部(新しい順)→二分探索。常に tombstone 生存フィルタ(同キー複数ペアでも生存は高々1)。初回スキャンは parent 解決を `finish()` の並列パスへ遅延。
- **既定除外(EXCLUDED)**: H/S 生属性+計算済み EXCLUDED ビット(自分または祖先が H|S)。クエリは既定でスキップ(`include_hidden_system` で解除)。継承はスキャン finish 時に O(n) 伝播、USN 挿入/移動時に親から再計算。制限: 除外ブランチからの subtree 移動は次回リスキャンまで陳腐化。
- **generation 2層**: `content_generation` は USN バッチ毎++(既存結果ハンドルは読み出し継続可)。`structural_generation` はコンパクション/フルリスキャン時のみ++(既存ハンドルはハードSTALE=`FMF_E_STALE`)。差し替えは必ず `VolumeSlot::install_index` 経由(旧値+1 を引き継ぐ。初回/スナップショット復元は bump しない)。スナップショットには非永続(プロセス内単調性で十分)。
- **クエリ時マテリアライズ**: ボリューム毎に順列を1パスフィルタ→ソート順確定済み連続配列+マルチボリューム k-way マージ(単一ボリュームは直コピー)。以後のページ取得は O(1) スライス。列クリック=別ソートで再発行。
- **インクリメンタル検索(クエリキャッシュ)**: `VolumeSlot::last_query` が直前の(compiled, options, 両generation, ids)を保持。`query/subsume.rs` の保守的包含規則(同一ソート・単一ANDグループ・needle包含/範囲縮小/フィルタ追加のみ。fold橋渡しは orig→folded 方向のみ)で証明可能に絞り込める場合、`query::refine` が前回 ids を完全評価でフィルタ — O(前回ヒット数)。正しさは oracle テスト(refine==fresh)、キルスイッチ `FMF_QUERY_CACHE=0`、観測 `QueryTrace.cache`。
- **ロック**: `parking_lot::RwLock`。検索=read、USNバッチ適用=write。index の書き手は volume スレッド1本。
- **スレッド**: 初回スキャン=ボリューム毎1スレッド。USN追従=ボリューム毎1スレッド(ブロッキング読み→吸い尽くし→バッチ適用)。停止は `CancelSynchronousIo`。
- **初回スキャン**: $MFT を16MiBチャンクでストリーミング読み(read-ahead スレッド1本+バッファ3本、起動失敗は逐次読みに劣化+カウンタ)、チャンク内は1MiBサブレンジで rayon 並列パース。チャンク順追記で EntryId 割当は逐次版と決定的一致(等価性ゲート=admin test)。deferred($ATTRIBUTE_LIST)名前は拡張レコードの RAM キャッシュから解決([ADR-0011](adr/0011-scan-streaming-pipeline.md))。
- **検索実行**: クエリ→AST→`CompiledTerm` 列(コスト順、AND短絡)。rayon 64kチャンク並列。スイープは常に lower_pool。大文字 needle / Sensitive は fold needle のスーパーセットスイープ+原文 residual 検証、residual は fold 同一エントリを O(1) で解決([ADR-0004](adr/0004-fold-overflow-name-layout.md))。`dm:` はローカルTZ。NFC/NFD 正規化はしない(既知制約)。trigram 索引は不採用([ADR-0002](adr/0002-linear-sweep-no-trigram.md))。
- **派生キャッシュ(OffsetTable/DirPaths/SizePerm/MtimePerm)**: content_generation 毎に世代管理、可能な限り前世代から差分延長(OffsetTable は stale 比 n/8 超でフル再構築、ウォーターマーク不整合は warn+カウンタ+再構築)。DirPaths は初回 path クエリで遅延構築、fold/orig 別スロット、dir-topology 世代が不変な限り差分延長。バイト数は `IndexStats.derived_cache_bytes` で B/entry ゲートに計上。
- **永続化**: `{index_dir}\{drive-letter}.fmfidx`(例 `c.fmfidx`)、形式 FMFIDX04([ADR-0010](adr/0010-snapshot-raw-pod-no-compat.md))。temp→`MoveFileEx(REPLACE_EXISTING)`。起動時: ロード→検証→USN再生→ライブ追従。失敗は常にフルリスキャンへ。

## FFI 契約(C ABI)

共通規約:
- DLL名 **`fmf_engine`**。全関数は `int32_t` ステータス(`FMF_OK=0`)+出力引数。
- 文字列は UTF-8(ファイル名は **WTF-8**: 不正サロゲートを保持。C#側は専用デコードでUTF-16へ復元)。
- ハンドルは opaque ポインタ。全関数スレッドセーフ。コールバック内からのFFI再入は禁止。
- 全入口で `catch_unwind` → `FMF_E_PANIC`。詳細メッセージは `fmf_last_error`(スレッドローカル)。

```c
// ── ライフサイクル ──
uint32_t fmf_abi_version(void);                         // 現在 1。C#側が起動時に照合
// config_json: { "index_dir": "...", "log_dir": "...", "log_level": "info" } (必須キー)
int32_t fmf_engine_create(const char* config_json, FmfEngineHandle* out);
int32_t fmf_engine_destroy(FmfEngineHandle h);          // 内部スレッドjoin+保存(明示保存は fmf_flush)

// ── イベント(エンジン内部スレッドから発火。受け側がDispatcherQueueへマーシャリング) ──
// kind: 1=Progress(volume, scanned) / 2=VolumeReady(volume, entries)
//       / 3=IndexChanged(エンジン側200msデバウンス、唯一のスロットル)
//       / 4=RescanStarted(volume) / 5=VolumeFailed(volume) / 6=EngineError(severity)
typedef void (*FmfEventCb)(const FmfEvent* ev /*POD*/, void* user);
int32_t fmf_set_event_callback(FmfEngineHandle h, FmfEventCb cb, void* user); // cb=NULLで解除

// ── ボリュームと索引 ──
int32_t fmf_list_volumes(FmfEngineHandle h, FmfVolumeStatus* buf, uint32_t cap, uint32_t* count);
int32_t fmf_index_start(FmfEngineHandle h, const char* const* volumes, uint32_t n); // 明示開始・非同期。要素はドライブラベル "C:"
int32_t fmf_index_status(FmfEngineHandle h, FmfVolumeStatus* buf, uint32_t cap, uint32_t* count);
// FmfVolumeStatus.state: Scanning / Ready / Rescanning / Failed
// クエリは常に「Ready なボリュームのみ」を対象に成功する(UIはstateで部分結果InfoBarを判定)

// ── クエリ(同期・高速。ソートはクエリ時に確定) ──
// options: { sort: Name|Size|Mtime, dir: Asc|Desc, case_mode: Smart|Insensitive|Sensitive,
//            include_hidden_system: bool(既定false=H/S属性とその配下を除外) }
int32_t fmf_query(FmfEngineHandle h, const char* query_utf8,
                  const FmfQueryOptions* options, FmfResultHandle* out, uint64_t* out_count,
                  FmfBlob** out_trace /* nullable: QueryTraceのJSON */);

// ── 可観測性(JSONブロブ。FmfPageと同じ「エンジン確保+free」パターン) ──
// FmfBlob { data: *const u8, len: u32 } — UTF-8 JSON
int32_t fmf_engine_stats(FmfEngineHandle h, FmfBlob** out); // MetricsSnapshot(直近トレース・ヒストグラム・USNフィード・カラム別メモリ)
int32_t fmf_blob_free(FmfBlob*);
// ── ページ取得: エンジン確保の連続ブロック(行ヘッダ配列+文字列blob)。P/Invoke 1回・コピー1回 ──
// FmfRow(48バイト・パディング無し。fmf-ffi の contract_tests が size/offset を固定):
//   { entry_ref u64, frn u64, size u64, mtime i64,
//     name_off u32, parent_path_off u32, flags u32, name_len u16, parent_path_len u16 } + 末尾blob
// 戻り FMF_E_STALE = structural_generation 不一致。UIは同一クエリを再発行
int32_t fmf_result_page(FmfResultHandle r, uint64_t offset, uint32_t count, FmfPage** out);
int32_t fmf_page_free(FmfPage* p);
int32_t fmf_result_free(FmfResultHandle r);

// ── 診断 ──
// len は in/out: in=バッファ容量、out=書いた長さ(NUL含まず)。容量不足は黙って切り詰め
// (常にNUL終端)。buf=NULL は必要サイズの照会。
int32_t fmf_last_error(char* buf, uint32_t* len);
```

エラーコード表(pipeプロトコルと共用。**追記のみ・再番号禁止** — contract_tests が値をピンする): `FMF_OK=0, FMF_E_INVALID_ARG=1, FMF_E_STALE=2, FMF_E_NOT_ADMIN=3, FMF_E_VOLUME=4, FMF_E_QUERY_SYNTAX=5, FMF_E_IO=6, FMF_E_LOCKED=7, FMF_E_PANIC=99`。
`FMF_E_LOCKED` = index_dir の writer lock を他プロセスが保持(単一書き手不変条件のプロセス間強制。「Pipe プロトコル」節参照)。

```c
// ── 明示保存(v2で実体化) ──
// 全Ready volumeのうちdirty(前回保存からcontent_generationが進んだ)なもののみ
// スナップショット保存。サービスが定期+停止時に内部で呼ぶ。pipeには公開しない
// (オペコード11は番号予約のみ — クライアント起動のflush連打はUSN適用を止めるDoS経路)。
int32_t fmf_flush(FmfEngineHandle h);
```

**意図的に入れないもの**: `fmf_entry_full_path`(行が name+parent_path を持つので不要)/ クエリキャンセル(クエリは数十ms想定。UIは世代カウンタで古い結果を捨てる。重くなったら `fmf_query_cancel` を追加する余地のみ残す)。

## Pipe プロトコル(v2 サービス分離)

`fmf-service`(特権サービス)と非特権UIの間のワイヤ仕様。本節が正本。ワイヤ型と
エンコード/デコードの実装は `fmf-proto`(rlib)に置き、fmf-ffi とは値ピンのテストで同期する
(cdylib は依存できないため定数は複製し、fmf-ffi の contract_tests が一致を固定する)。

### トランスポート

- pipe名: `\\.\pipe\fmf-engine-v1`(プロトコル版数を名前に含む。非互換変更は名前ごと上げる)
- バイトモード(`PIPE_TYPE_BYTE`)+長さ前置きフレーミング(メッセージモードは使わない)
- 作成フラグ: **初回インスタンスのみ** `FILE_FLAG_FIRST_PIPE_INSTANCE`(名前の先取りを検知。
  2本目以降は同一SDDLでフラグ無し — サーバが初回インスタンスを保持する限りスクワッティング不能)
  + 全インスタンスに `PIPE_REJECT_REMOTE_CLIENTS`。インスタンス上限 8(超過は接続拒否+
  `pipe_connections_rejected` カウンタ)
- DACL: 明示SDDL `D:P(A;;GA;;;SY)(A;;GRGW;;;<利用者SID>)` — SYSTEM と install 時に捕捉した利用者SIDのみ。
  Authenticated Users は不採用(マルチユーザー機での名前漏洩)。Administrators 許可も不成立
  (UACフィルタ済みトークンでは deny-only になり非昇格UIが接続できない)。多重防御として
  接続受理時にクライアントトークンを `service.json` の `authorized_sids` と照合する
- クライアント側検証: 既定pipe名のとき `GetNamedPipeServerProcessId` → サーバトークンが SYSTEM
  であることを確認(偽サーバ対策)。`--pipe-name` 指定時(テスト)は検証をスキップ

### フレーム(16バイトLEヘッダ+ペイロード)

```c
struct FrameHeader {            // 16 bytes, little-endian
    uint32_t len;               // ペイロード長(ヘッダ含まず)。上限 16 MiB
    uint16_t opcode;            // 下表
    uint16_t flags;             // bit0=response, bit1=event push
    uint32_t request_id;        // リクエスト/レスポンス相関。event push は 0
    int32_t  status;            // レスポンスのみ有効。エラーコード表(FFIと共用)
};
```

- 不正フレーム(未知opcode・len超過・切詰め)= 接続切断+`pipe_malformed_frames` カウンタ+warn
- エラー応答(status != 0)はペイロードに UTF-8 詳細を同梱(`fmf_last_error` の写像 —
  スレッドローカルの pull は pipe には存在しない)
- リクエストは request_id で多重化(out-of-order 完了可)

### オペコード表(FFI関数との対応)

ペイロード表記の凡例: 型注釈つき `{}` = **リトルエンディアン・パディング無しのPODバイト列**。
「JSON」= UTF-8 JSON、**フィールド名は snake_case(serde 既定)**。POD+可変長データは記載順に
隙間なく連結。ボリューム識別子は全箇所で**ドライブラベル文字列 `"C:"`**(GUIDは使わない)。
バイナリ・JSON とも代表メッセージは Rust/C# 両スイートで同一の**ゴールデンフレーム**(バイト列)
としてピンする。

| op | 名前 | FFI対応 | ペイロード(req → resp) |
|---|---|---|---|
| 1 | Hello | `fmf_abi_version` | `{protocol_version:u32}` → `{protocol_version:u32, abi_version:u32, server_pid:u32}`(版不一致は INVALID_ARG+切断) |
| 2 | Subscribe | `fmf_set_event_callback(cb≠NULL)` | 空 → 空。以後この接続にイベントをプッシュ |
| 3 | Unsubscribe | `fmf_set_event_callback(NULL)` | 空 → 空 |
| 4 | ListVolumes | `fmf_list_volumes` | 空 → JSON `[{"volume":"C:","state":0,"entries":0}]`(state は FmfVolumeStatus.state と同値) |
| 5 | IndexStart | `fmf_index_start` | JSON `{"volumes":["C:"]}` → 空(service.json へ永続化) |
| 6 | IndexStatus | `fmf_index_status` | 空 → JSON(ListVolumes と同形) |
| 7 | Query | `fmf_query` | `FmfQueryOptions`(下記16B POD)+UTF-8クエリ文字列(長さはフレーム len から導出・NUL終端なし) → `{result_id:u64, count:u64}`+QueryTrace JSON |
| 8 | ResultPage | `fmf_result_page` | `{result_id:u64, offset:u64, count:u32}` → `{row_count:u32, blob_len:u32}` → `FmfRow`(48B)× row_count(密配置) → 文字列blob(blob_len バイト・WTF-8)。`name_off`/`parent_path_off` は **blob 先頭基準**のバイトオフセット(FFI の FmfPage と同一レイアウト) |
| 9 | ResultFree | `fmf_result_free` | `{result_id:u64}` → 空 |
| 10 | Stats | `fmf_engine_stats` | 空 → MetricsSnapshot JSON(FFIと同一形状・snake_case) |
| 11 | (Flush 予約) | `fmf_flush` | **番号予約のみ・実装しない** — クライアント起動の flush 連打は index.read() 保持の反復で USN 適用を止めるローカル DoS 経路。保存はサービス内部の責務 |
| 12 | ServiceInfo | (サービス固有) | 空 → JSON `{uptime_ms, connections, version}` |

`FmfQueryOptions`(16B・パディング無し・LE — FmfRow と同様に contract test でピン):
`{ sort:u32@0(0=Name 1=Size 2=Mtime), desc:u32@4(0=Asc 1=Desc),
case_mode:u32@8(0=Smart 1=Insensitive 2=Sensitive), include_hidden_system:u32@12(0/1) }`

写像の例外(C ABI 固有で pipe に存在しないもの): `fmf_engine_create`/`fmf_engine_destroy`
(接続確立/切断とサービス寿命に吸収)、`fmf_page_free`/`fmf_blob_free`(フレーム受信で所有権が
クライアントへ移る)、`fmf_last_error`(エラー応答のインライン詳細)。

### イベントプッシュ

- Subscribe 済み接続へ `flags=event, request_id=0, opcode=イベント種`(FFI の kind 1〜6 と同値)で
  `FmfEvent` 相当 POD `{kind:u32, _pad:u32, entries:u64, volume:[u8;16]}` をプッシュ。
  `volume` は **UTF-8 ドライブラベル("C:")の 0x00 詰め**(GUIDではない)
- 接続毎に有界キュー(256)+専用 writer スレッド。満杯は最古をドロップ+`pipe_events_dropped`
  カウンタ+warn — 遅い/読まないクライアントが volume スレッドを絶対にブロックしない(固まらない)。
  ドロップは IndexChanged 系なら次回再クエリで自己回復する
- イベントフレームは opcode にイベント種(1〜6)を載せるため要求オペコードと番号が重なる —
  **必ず flags の event ビットで先に弁別する**こと(opcode 単独で dispatch しない)
- クライアントの(再)接続シーケンスは固定(本節が正本): **Hello → Subscribe → IndexStatus →
  IndexChanged 強制発火**。最後の IndexChanged は**クライアントがローカルで合成する**
  (サーバは送信しない)— 切断中に取り逃した変更を再クエリで取り込むため

### 結果ハンドル(result_id)の寿命

- サーバは接続毎レジストリで `ResultSet` を保持。`ResultFree` か切断で解放
- 上限 64/接続。超過は**最終アクセスが最古のもの(LRU)** を evict し、以後その result_id への
  ResultPage は `FMF_E_STALE`(detail に "evicted" を含め、構造的世代交代と弁別可能にする)。
  クライアントは既存の STALE→再クエリ経路で回復する

### 単一書き手の排他(プロセス間)

- `Engine::new` は `{index_dir}\.writer.lock` を共有モード0で開き生存期間中保持。失敗は
  `FMF_E_LOCKED`。OSハンドル消滅で自動解放されるため stale ロックは発生しない
- サービスが負けた側(in-proc UI が先行): バックオフ付きリトライ(5s→60s上限)+保持プロセス pid を
  ログ。SCM の障害回復(再起動)ループを誘発しない終了コードで停止する
- UI が負けた側(サービス稼働中に `--engine=inproc`): 説明付き InfoBar(「サービス稼働中。
  in-proc を使うには `just service-stop`」)

### マシン単位設定 `%ProgramData%\find-my-files\service.json`(サービス所有)

```json
{ "volumes": ["C:"], "log_level": "info", "flush_interval_secs": 300, "authorized_sids": ["S-1-5-21-…"] }
```

- `fmf-service install` が利用者SID捕捉と共に生成。IndexStart 受信で volumes を永続化。
  初回既定は全固定NTFSボリューム
- ユーザー単位の `%APPDATA%\find-my-files\settings.json`(UI所有)とは所有権を分離する

## C# 側の契約

- `IEngineClient`(差し替え境界): `SearchAsync(query, options) → SearchOutcome(ISearchResult, QueryTrace)` / `GetStatsAsync` / `ListVolumesAsync` / `StartIndexingAsync` / `GetStatusAsync`(**3メソッドは v2 で Task 返しに変更** — pipe 越えの同期呼び出しはUIスレッドの「固まらない」違反)/ `event IndexChanged` / `event VolumeUpdated` / `event EngineErrorOccurred` / `EngineConnectionState Connection { get; }` + `event ConnectionChanged`(InProc | Connecting | Connected | Reconnecting。Ffi/Fake は InProc 固定)。Fake/FFI/Pipe の3実装が同じ口に従う。
- **エンジン選択**(`EngineClientFactory`): CLI `--fake-engine` / `--engine=pipe|inproc` > settings.json の `"engine"`(既定 `auto`)> auto = pipe 250ms プローブ → 成功で Pipe / 失敗かつプロセス昇格済みで Ffi / どちらも不可なら説明付き InfoBar+Fake フォールバック+「管理者として再起動」ボタン(明示操作のみ。自動 runas ループ禁止)。
- **切断と再接続**(`PipeEngineClient`): 切断 = 進行中要求を `EngineUnavailableException` で即時失敗・生存 `ISearchResult` を epoch 無効化(以後 `GetRangeAsync` → `StaleResultException` = 既存の再クエリ機構が回復経路)・バックオフ(250ms→5s)で無限再接続。再接続シーケンスは「Pipe プロトコル」節が正本(`VolumeUpdated` 群は IndexStatus 応答から合成して発火)。要求には既定タイムアウト10s。
- `SearchResultHandle : SafeHandle`。ページフェッチは `DangerousAddRef/Release` を挟み、`Dispose()` 後も in-flight フェッチ完了まで実体を解放しない。
- ページ受領→`ResultRow` へコピー→**即 `fmf_page_free`**。
- コールバック delegate はクライアントのフィールドに保持(GC回収防止)。受領後 `DispatcherQueue.TryEnqueue` でUIへ。
- **検索パイプラインの責務分割**(MainViewModel は合成ルートのみ):
  - `SearchOrchestrator` — いつ・何を検索するか: 50msデバウンス(クリアは即時)、generationカウンタによる陳腐結果のDispose、`RequeryOrigin` 分類、Stale有界リトライ(1回)、例外分類。**空クエリはエンジンに投げない**(空欄に返すべき結果はない、というプロダクト規則。match-all列挙はUSNティック毎にIDが動くため起動画面が永遠に再描画される)— `PresentEmpty`(冪等)で空画面。**IME変換中はクエリ保留**(`TextCompositionStarted/Ended`、確定文字列だけが通常デバウンスで流れる)。
  - `ResultsPresenter` — 結果の提示: 公開**前**に可視範囲ページをプリフェッチし、`VirtualResultList.Reassign` で原子的に公開(旧結果は新結果が揃うまで画面に残る=空白フレームゼロ)。件数テキストと viewport 配置イベント。
- 再クエリの2系統(`RequeryOrigin` が分類): **タイプ/クリア/ソート/フィルタ起因=先頭リセット** / **IndexChanged/VolumeReady/Stale起因=先頭可視インデックスを退避→復元、選択はseed内EntryRef一致時のみベストエフォート復元**。
- `VirtualResultList`(非ジェネリックIList+INCC+IItemsRangeInfo): **ページと同寿命の単一インスタンス**(ItemsSource は x:Bind OneTime — 差し替えるとListViewの仮想化状態が破棄されてちらつく)。新結果は `Reassign(result, seeds)` = epoch++ → ページキャッシュ破棄 → seed適用 → **INCC Reset を1回発行**(UIスレッド限定)。**同一結果の再クエリ**(エンジンが`QueryTrace.unchanged`で保証: 同一テキスト+オプションかつ全ボリュームでID列がmemcmp一致)は `RefreshInPlace` = epoch++ → ハンドル差し替え → 可視seedを既存行インスタンスにin-place充填(MVVMセッターは値変化時のみ通知)→ **Resetなし・件数テキスト不変** — アイドルのUSNトラフィック(ログ・テレメトリ等)が200ms毎に引き起こす再クエリで画面が再描画されない。in-place更新されたsize/mtimeは値が変わったセルだけ更新される。indexer は絶対にフェッチせずプレースホルダ返却(**範囲外は即throw** — 負indexや偽ページ捏造をしない)。`RangesChanged` で可視範囲±1ページを64行単位バックグラウンドフェッチ→既存 ResultRow のプロパティ充填。旧epochのフェッチ完了は黙って破棄。ページLRU上限4096行。ハードSTALE受領→`BecameStale`(epoch一致時のみ)→ Orchestrator が再クエリ。
- **IList契約の不変条件(在籍を偽って肯定しない)**: XAMLはWinRTアダプタ経由で `Contains`/`IndexOf`/`GetAt` の答えを盲信する。偽の「不在」はコンテナ再実体化で済むが、偽の「在籍」は `GetAt(staleIndex)` でXAML深部のクラッシュになる(実証済み: 結果ありの検索→全消去で確実に再現した `Int32.MaxValue-1` 例外の根)。在籍の定義=「indexがCount未満 かつ 現在のページキャッシュの該当スロットがその同一インスタンス」。旧結果の行・LRU追い出し済みページの行・列挙用の一時行は常に不在と答える。列挙/CopyToは仮想化状態(LRU)を乱さない。変異系(Reassign/RefreshInPlace)のUIスレッド検査はRelease常時有効。

## エラーハンドリングと診断(原則:「落ちない・固まらない・黙らない」)

全異常は3経路に必ず届く: **①ログファイル ②diagリング(=F12パネル/fmf statsに自動表示) ③UIのInfoBar**。テレメトリ送信はしない(ローカルのみ)。

- **ログ**: エンジン=`%ProgramData%\find-my-files\logs\engine.log`(日次ローテーション、`FMF_LOG`環境変数でフィルタ)、アプリ=`%APPDATA%\find-my-files\logs\app.log`(2MBで1世代ローテーション)
- **diagリング**(fmf-core::diag): WARN以上のtracingイベント+panic(バックトレース付き)を直近128件保持。`MetricsSnapshot.recent_errors`に常時含まれる
- **panic**: グローバルフックで捕捉→ログ+リング。volume threadは`catch_unwind`の防火壁付きで、panicしてもUIには必ず`VolumeFailed`が届く(無言のハングは起きない)
- **イベント種6 `FMF_EVENT_ENGINE_ERROR`**: diagイベント発生のPOD通知(entries=severity 1=warn/2=error/3=panic)。詳細テキストはstats JSONからpull(push通知+pull詳細)
- **劣化カウンタ**(`MetricsSnapshot.counters`、0でなければF12に表示): stat_fetch_failures / usn_batches_truncated / snapshot_load_failures / snapshot_save_failures / deferred_names_unresolved / corrupt_mft_records / journal_rescans / scan_pipeline_fallbacks(スキャンのread-ahead I/Oスレッド起動失敗→逐次読みに劣化)/ offset_table_rebuild_fallbacks(オフセットテーブルのウォーターマーク不整合→フル再構築に劣化)/ lazy_perm_rebuild_fallbacks(遅延ソート順列の同種防御)/ compaction_aborts(コンパクション中の世代不整合→コピー破棄。単一書き手不変条件の破れ検知)/ pipe_malformed_frames(不正フレーム→接続切断)/ pipe_events_dropped(イベント有界キュー溢れ→最古ドロップ)/ pipe_connections_rejected(インスタンス上限超過)
- **C#規約**: fire-and-forgetは必ず `task.Forget(area)`(例外→app.log+InfoBar)。シェル操作は`ShellOps`経由。グローバル例外ハンドラがクラッシュマーカーを書き、次回起動時に通知
- **診断コピー**: F12パネルの「診断情報をコピー」= stats JSON+app.log末尾+環境情報

| FFIコード | 意味 | UI挙動 | リトライ |
|---|---|---|---|
| FMF_E_QUERY_SYNTAX(5) | クエリ構文エラー | ステータスバーに表示 | 入力修正 |
| FMF_E_STALE(2) | 構造的世代交代 | 同一クエリ自動再発行 | 自動 |
| FMF_E_NOT_ADMIN(3) | 昇格不足 | InfoBar+説明 | 再起動 |
| FMF_E_LOCKED(7) | index_dir を他エンジンが保持 | InfoBar+説明(「サービス稼働中。in-proc は just service-stop 後に」) | サービス停止後に再起動 |
| FMF_E_PANIC(99) | エンジン内panic | InfoBar+engine.log誘導 | 不可(報告) |
| その他(1,4,6) | 引数/ボリューム/IO | InfoBar | 場合による |

## 遅延予算(変更→画面反映 ≤1s のAC内訳)

USNバッチ確定 ≤100ms + エンジンIndexChangedデバウンス 200ms(唯一のスロットル)+ UI再クエリ ≤100ms + 描画 ≤100ms = **≤500ms**(2倍余裕)。UI側に追加スロットルを置かないこと。

pipe 経路の追加予算(**正本はここ** — 他文書の数値は本節を参照): ResultPage 64行の往復 p99
**≤5ms**(暫定 — ループバック結合テストがアサートし、実測で確定する)。F12 の `PageRttEwma` で
常時観測。イベントプッシュは上記デバウンス後のワンホップで、予算構造は変わらない。

pipe のテストゲート: プロトコル round-trip とループバック結合(一意pipe名+
`insert_ready_volume`)は非昇格の `cargo test` で無条件実行。C#クライアント×実 fmf-service の
結合は `FMF_PIPE_TESTS=1`(`just test-pipe`)。実ボリュームを使うサービスE2Eは従来どおり
`FMF_ADMIN_TESTS=1`(昇格)。
