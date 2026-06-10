# アーキテクチャと FFI 正本契約

このファイルが **FFI契約の正本**。エンジン(Rust)とUI(C#)の双方はここに従う。シグネチャ変更はまずこのファイルを更新してから両側を直す。

## 全体構成

```
┌─────────────────────────────────────────────┐
│ WinUI 3 アプリ (C#/.NET, requireAdministrator) │
│   ViewModels ── IEngineClient (差し替え境界)   │
│        ├─ FfiEngineClient ── P/Invoke          │
│        └─ FakeEngineClient (--fake-engine)     │
└────────────────────┬────────────────────────┘
                     │ C ABI (in-proc)
┌────────────────────▼────────────────────────┐
│ fmf_engine.dll (fmf-ffi crate, cdylib)        │
│   変換・ハンドル管理・catch_unwind のみ        │
├──────────────────────────────────────────────┤
│ fmf-core (rlib): VolumeIndex / query /        │
│   mft scan (ntfs-reader) / usn tail / persist │
└──────────────────────────────────────────────┘
```

v2(サービス分離)では `fmf-service` crate(fmf-core を再利用)+ `PipeEngineClient` を追加。
**FFI 1関数 = pipe 1メッセージ、イベントコールバック = pipe プッシュ通知**に写像できるよう、本契約の境界を維持する。

## モジュールマップ(1ファイル=1責務)

```
fmf-core/src/
├─ index/        mod(型+再エクスポート) / core(VolumeIndex+読み取り+派生キャッシュ)
│                / mutate(USNミューテーション) / snapshot(永続化、unsafe POD はここに封じ込め)
│                / builder(2パス構築+EXCLUDED伝播) / testutil
├─ query/        mod(AST/compile 公開面) / exec(searchドライバループ+materialize)
│                / sweep(pool-sweep候補生成) / matchers(残余評価) / memo(DirPaths/OffsetTable)
├─ engine/       mod(Engine+ライフサイクル+イベント) / volume(VolumeSlot+スレッド防火壁+install_index)
│                / search(ボリューム横断+k-wayマージ) / results(ResultSet+STALE判定) / tests
├─ mft.rs / scan.rs / usn/{records,apply,session} / metrics.rs / diag.rs / wtf8.rs
fmf-ffi/src/     lib(エラーコード+再エクスポート+エクスポートピン) / error / handle / events
                 / volumes / blob / results / contract_tests(ABIレイアウト・null・エラー経路の固定)
app/FindMyFiles/
├─ Engine/       IEngineClient(境界) / FfiEngineClient / FakeEngineClient / NativeEngine(P/Invoke)
├─ ViewModels/   MainViewModel(合成ルート) / SearchOrchestrator / ResultsPresenter
│                / NotificationCenter / PerfPanelViewModel / StatusFormatter / ResultRow
├─ Virtualization/ VirtualResultList(生涯単一+Reassign/epoch)
├─ Services/     IDispatcher(テストシーム) / DispatcherQueueDispatcher / Notifier / FileLog / ShellOps
└─ FindMyFiles.Tests/  xUnit(ManualDispatcher fake で決定的にUIスレッド模倣)
```

新フィールド・新メソッドの可視性は「その責務のディレクトリ内」を既定とする(`pub(super)`)。crate 外公開は mod.rs の `pub use` 経由のみ。

## エンジン内部の要点

- **VolumeIndex(ボリューム毎、struct-of-arrays)**: 名前プール2本(表示用 WTF-8 原文+simple case fold 済み検索用。オフセット同期)/ name_off(u32)+name_len(u16) / parent(EntryId=u32) / size(u64) / mtime(i64, FILETIME) / frn(u64) / flags(is_dir|tombstone|reparse) / FRN→EntryId マップ / fast sort 用順列配列3本(name/size/mtime)。パス文字列は保持せず親チェーンで遅延構築。削除は tombstone、閾値超でコンパクション。
- **既定除外(EXCLUDED)**: flagsにH(hidden)/S(system)の生属性と、計算済みEXCLUDEDビット(自分がH|S、または祖先にH|Sがいる)を保持。クエリは既定でEXCLUDEDをスキップ(`include_hidden_system`で解除)。継承はスキャンfinish時にO(n)伝播、USN挿入/移動時に親から再計算。**制限**: 除外ブランチからのsubtree移動では配下のビットが次回リスキャンまで陳腐化(dir-rename制限と同類)。
- **generation 2層**:
  - `content_generation`: USNバッチ適用毎に++。既存結果ハンドルは**読み出し継続可**(tombstone+末尾追記なので安全。削除済みが残る/新規が出ないだけの Everything 同等の結果整合)。
  - `structural_generation`: コンパクション/フルリスキャン時のみ++。既存ハンドルは**ハードSTALE**(`FMF_E_STALE`)。実装: index の差し替えは必ず `VolumeSlot::install_index` を通り、旧 index の値+1 を新 index に引き継ぐ(初回インストール/スナップショット復元は bump しない)。スナップショットはこの値を**永続化しない**(復元時0)— 結果ハンドルはプロセスを跨がないため、プロセス内の単調性で十分。
- **クエリ時マテリアライズ**: `fmf_query` がボリューム毎の該当順列を1パスフィルタし、ソート順確定済みの連続配列 `Vec<u64>`(volume_id<<32|entry_id 相当)に確定+マルチボリューム k-way マージ(単一ボリュームは直コピーのfast path)。以後のページ取得は **O(1) スライス**。列クリック=同一クエリを別ソートで再発行。
- **インクリメンタル検索(クエリキャッシュ)**: `VolumeSlot::last_query` がボリューム毎に直前の(compiled, options, 両generation, ids)を保持。新クエリが前回を**証明可能に**絞り込み(`query/subsume.rs`の保守的包含規則: 同一ソート・単一ANDグループ・needle包含/範囲縮小/フィルタ追加のみ。fold橋渡しはorig→folded方向のみ健全)、かつ両generationが不変なら、`query::refine` が前回idsを完全評価でフィルタ — **O(前回ヒット数)**でスキャンもO(n)マテリアライズも省略(Everything本家の核心技法)。USNバッチはgeneration++で暗黙に無効化、構造交代は`install_index`で明示クリア。正しさはoracleプロパティテスト(refine==fresh search)で担保、キルスイッチ=`FMF_QUERY_CACHE=0`、観測=`QueryTrace.cache`(miss/refine/partial)。FFI/C#は完全無変更。
- **ロック**: `parking_lot::RwLock`。検索=read、USNバッチ適用=write(ms級)。
- **スレッド**: 初回スキャン=ボリューム毎1スレッド並列。USN追従=ボリューム毎1スレッド(ブロッキング読み→吸い尽くし→バッチ適用)。停止は `CancelSynchronousIo`。
- **初回スキャンの内部並列**: $MFTを16MiBチャンクでストリーミング読みしつつ、(a) 専用I/OスレッドがチャンクN+1を先読み(read-aheadパイプライン、バッファ3本で上限RAM固定。スレッド起動失敗は`scan_pipeline_fallbacks`カウンタ+逐次読みに劣化)、(b) チャンク内はレコード境界1MiBサブレンジでrayon並列パース(fixup→属性抽出→WTF-8変換までワーカー側)。ワーカーバッチをチャンク順に追記するためEntryId割当は逐次版と決定的に一致(等価性ゲート=admin test `streaming_scan_matches_reference`)。
- **検索実行**: クエリ→AST→`CompiledTerm` 列(コスト順: 数値フィルタ→memmem→wildcard/regex、AND短絡)。rayonで64kチャンク並列。smart case(全小文字needle→lower_pool、大文字含む→name_pool)。`dm:` はローカルTZ解釈。NFC/NFD正規化はしない(既知制約)。
- **派生キャッシュ(OffsetTable/DirPaths)**: content_generation毎に世代管理。OffsetTableはプール追記が常に末尾である性質を使い、世代遷移で**ソートなしの差分延長**(前世代テーブルを可能ならin-place再利用+追記分マージ。in-place dirリネームの旧ペアはstaleとして残し、sweepが`name_off`照合でスキップ)。stale比がn/8超で正常な方針切替としてフル再構築。ウォーターマーク不整合(起きないはずの状態)は warn+`offset_table_rebuild_fallbacks`+フル再構築。DirPathsはprewarmせず初回pathクエリで遅延構築(合成1M実測24ms。pathクエリを使わないセッションのRAMをディレクトリ全パス×2プール分節約)。両キャッシュのバイト数は `IndexStats.derived_cache_bytes` としてB/entryゲートの分母に計上。
- **n-gram(trigram)索引は不採用(2026-06判定・数値根拠つき)**: 合成1M件でのコールド3文字クエリは約2.9ms(クエリキャッシュMISS+派生キャッシュwarm、materialize込み)で、判定基準「per-volume scan_us p99 > 25ms @1M」を一桁下回る。線形プールスイープ(SIMD memmem)+インクリメンタル検索+OffsetTable差分延長で予算内のため、posting維持コスト(RAM ≤110B/file制約下で+10〜15B/file、USNバッチ毎の差分維持)に見合わない。**再検討トリガ**(全て満たす場合のみ): (1)キャッシュMISS時コールド3文字 scan_us p99>25ms@1M (2)`fmf stats --trigram-estimate` の実測見積もり≤15B/fileかつ合計≤110B/file (3)posting差分維持≤2ms/バッチ (4)1ボリューム400万件超の実需要。
- **永続化**: `{index_dir}\{volume-guid}.fmfidx`。自前バイナリ(magic+version+JournalID+最終USN+セクションテーブル+生列ダンプ+xxhash64)。temp→`MoveFileEx(REPLACE_EXISTING)`。起動時: ロード→検証→USN再生で追いつき→ライブ追従。失敗は常にフルリスキャンへ。

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
int32_t fmf_engine_destroy(FmfEngineHandle h);          // 内部スレッドjoin+保存(明示flushはMVPでは無し)

// ── イベント(エンジン内部スレッドから発火。受け側がDispatcherQueueへマーシャリング) ──
// kind: 1=Progress(volume, scanned) / 2=VolumeReady(volume, entries)
//       / 3=IndexChanged(エンジン側200msデバウンス、唯一のスロットル)
//       / 4=RescanStarted(volume) / 5=VolumeFailed(volume) / 6=EngineError(severity)
typedef void (*FmfEventCb)(const FmfEvent* ev /*POD*/, void* user);
int32_t fmf_set_event_callback(FmfEngineHandle h, FmfEventCb cb, void* user); // cb=NULLで解除

// ── ボリュームと索引 ──
int32_t fmf_list_volumes(FmfEngineHandle h, FmfVolumeStatus* buf, uint32_t cap, uint32_t* count);
int32_t fmf_index_start(FmfEngineHandle h, const char* const* volume_guids, uint32_t n); // 明示開始・非同期
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

エラーコード表(v2 pipeプロトコルと共用): `FMF_OK=0, FMF_E_INVALID_ARG=1, FMF_E_STALE=2, FMF_E_NOT_ADMIN=3, FMF_E_VOLUME=4, FMF_E_QUERY_SYNTAX=5, FMF_E_IO=6, FMF_E_PANIC=99`。

**MVPで意図的に入れないもの**: `fmf_entry_full_path`(行が name+parent_path を持つので不要)/ クエリキャンセル(クエリは数十ms想定。UIは世代カウンタで古い結果を捨てる。重くなったら `fmf_query_cancel` を追加する余地のみ残す)/ `fmf_flush`(明示保存。destroy が join+保存を行うため不要。サービス化(v2)で必要になれば追加)。

## C# 側の契約

- `IEngineClient`(差し替え境界): `Search(query, options) → ISearchResult(Count, GetRangeAsync)` / `event IndexChanged` / `event IndexProgress` / `ListVolumes` / `StartIndexing`。Fake/FFI/将来Pipeの3実装が同じ口に従う。
- `SearchResultHandle : SafeHandle`。ページフェッチは `DangerousAddRef/Release` を挟み、`Dispose()` 後も in-flight フェッチ完了まで実体を解放しない。
- ページ受領→`ResultRow` へコピー→**即 `fmf_page_free`**。
- コールバック delegate はクライアントのフィールドに保持(GC回収防止)。受領後 `DispatcherQueue.TryEnqueue` でUIへ。
- **検索パイプラインの責務分割**(MainViewModel は合成ルートのみ):
  - `SearchOrchestrator` — いつ・何を検索するか: 50msデバウンス(クリアは即時)、generationカウンタによる陳腐結果のDispose、`RequeryOrigin` 分類、Stale有界リトライ(1回)、例外分類。
  - `ResultsPresenter` — 結果の提示: 公開**前**に可視範囲ページをプリフェッチし、`VirtualResultList.Reassign` で原子的に公開(旧結果は新結果が揃うまで画面に残る=空白フレームゼロ)。件数テキストと viewport 配置イベント。
- 再クエリの2系統(`RequeryOrigin` が分類): **タイプ/クリア/ソート/フィルタ起因=先頭リセット** / **IndexChanged/VolumeReady/Stale起因=先頭可視インデックスを退避→復元、選択はseed内EntryRef一致時のみベストエフォート復元**。
- `VirtualResultList`(非ジェネリックIList+INCC+IItemsRangeInfo): **ページと同寿命の単一インスタンス**(ItemsSource は x:Bind OneTime — 差し替えるとListViewの仮想化状態が破棄されてちらつく)。新結果は `Reassign(result, seeds)` = epoch++ → ページキャッシュ破棄 → seed適用 → **INCC Reset を1回発行**(UIスレッド限定)。indexer は絶対にフェッチせずプレースホルダ返却。`RangesChanged` で可視範囲±1ページを64行単位バックグラウンドフェッチ→既存 ResultRow のプロパティ充填。旧epochのフェッチ完了は黙って破棄。ページLRU上限4096行。ハードSTALE受領→`BecameStale`(epoch一致時のみ)→ Orchestrator が再クエリ。

## エラーハンドリングと診断(原則:「落ちない・固まらない・黙らない」)

全異常は3経路に必ず届く: **①ログファイル ②diagリング(=F12パネル/fmf statsに自動表示) ③UIのInfoBar**。テレメトリ送信はしない(ローカルのみ)。

- **ログ**: エンジン=`%ProgramData%\find-my-files\logs\engine.log`(日次ローテーション、`FMF_LOG`環境変数でフィルタ)、アプリ=`%APPDATA%\find-my-files\logs\app.log`(2MBで1世代ローテーション)
- **diagリング**(fmf-core::diag): WARN以上のtracingイベント+panic(バックトレース付き)を直近128件保持。`MetricsSnapshot.recent_errors`に常時含まれる
- **panic**: グローバルフックで捕捉→ログ+リング。volume threadは`catch_unwind`の防火壁付きで、panicしてもUIには必ず`VolumeFailed`が届く(無言のハングは起きない)
- **イベント種6 `FMF_EVENT_ENGINE_ERROR`**: diagイベント発生のPOD通知(entries=severity 1=warn/2=error/3=panic)。詳細テキストはstats JSONからpull(push通知+pull詳細)
- **劣化カウンタ**(`MetricsSnapshot.counters`、0でなければF12に表示): stat_fetch_failures / usn_batches_truncated / snapshot_load_failures / snapshot_save_failures / deferred_names_unresolved / corrupt_mft_records / journal_rescans / scan_pipeline_fallbacks(スキャンのread-ahead I/Oスレッド起動失敗→逐次読みに劣化)/ offset_table_rebuild_fallbacks(オフセットテーブルのウォーターマーク不整合→フル再構築に劣化)
- **C#規約**: fire-and-forgetは必ず `task.Forget(area)`(例外→app.log+InfoBar)。シェル操作は`ShellOps`経由。グローバル例外ハンドラがクラッシュマーカーを書き、次回起動時に通知
- **診断コピー**: F12パネルの「診断情報をコピー」= stats JSON+app.log末尾+環境情報

| FFIコード | 意味 | UI挙動 | リトライ |
|---|---|---|---|
| FMF_E_QUERY_SYNTAX(5) | クエリ構文エラー | ステータスバーに表示 | 入力修正 |
| FMF_E_STALE(2) | 構造的世代交代 | 同一クエリ自動再発行 | 自動 |
| FMF_E_NOT_ADMIN(3) | 昇格不足 | InfoBar+説明 | 再起動 |
| FMF_E_PANIC(99) | エンジン内panic | InfoBar+engine.log誘導 | 不可(報告) |
| その他(1,4,6) | 引数/ボリューム/IO | InfoBar | 場合による |

## 遅延予算(変更→画面反映 ≤1s のAC内訳)

USNバッチ確定 ≤100ms + エンジンIndexChangedデバウンス 200ms(唯一のスロットル)+ UI再クエリ ≤100ms + 描画 ≤100ms = **≤500ms**(2倍余裕)。UI側に追加スロットルを置かないこと。
