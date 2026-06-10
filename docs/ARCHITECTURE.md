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

## エンジン内部の要点

- **VolumeIndex(ボリューム毎、struct-of-arrays)**: 名前プール2本(表示用 WTF-8 原文+simple case fold 済み検索用。オフセット同期)/ name_off(u32)+name_len(u16) / parent(EntryId=u32) / size(u64) / mtime(i64, FILETIME) / frn(u64) / flags(is_dir|tombstone|reparse) / FRN→EntryId マップ / fast sort 用順列配列3本(name/size/mtime)。パス文字列は保持せず親チェーンで遅延構築。削除は tombstone、閾値超でコンパクション。
- **既定除外(EXCLUDED)**: flagsにH(hidden)/S(system)の生属性と、計算済みEXCLUDEDビット(自分がH|S、または祖先にH|Sがいる)を保持。クエリは既定でEXCLUDEDをスキップ(`include_hidden_system`で解除)。継承はスキャンfinish時にO(n)伝播、USN挿入/移動時に親から再計算。**制限**: 除外ブランチからのsubtree移動では配下のビットが次回リスキャンまで陳腐化(dir-rename制限と同類)。
- **generation 2層**:
  - `content_generation`: USNバッチ適用毎に++。既存結果ハンドルは**読み出し継続可**(tombstone+末尾追記なので安全。削除済みが残る/新規が出ないだけの Everything 同等の結果整合)。
  - `structural_generation`: コンパクション/フルリスキャン時のみ++。既存ハンドルは**ハードSTALE**(`FMF_E_STALE`)。
- **クエリ時マテリアライズ**: `fmf_query` がボリューム毎の該当順列を1パスフィルタし、ソート順確定済みの連続配列 `Vec<u64>`(volume_id<<32|entry_id 相当)に確定+マルチボリューム k-way マージ。以後のページ取得は **O(1) スライス**。列クリック=同一クエリを別ソートで再発行。
- **ロック**: `parking_lot::RwLock`。検索=read、USNバッチ適用=write(ms級)。
- **スレッド**: 初回スキャン=ボリューム毎1スレッド並列。USN追従=ボリューム毎1スレッド(ブロッキング読み→吸い尽くし→バッチ適用)。停止は `CancelSynchronousIo`。
- **検索実行**: クエリ→AST→`CompiledTerm` 列(コスト順: 数値フィルタ→memmem→wildcard/regex、AND短絡)。rayonで64kチャンク並列。smart case(全小文字needle→lower_pool、大文字含む→name_pool)。`dm:` はローカルTZ解釈。NFC/NFD正規化はしない(既知制約)。
- **永続化**: `{index_dir}\{volume-guid}.fmfidx`。自前バイナリ(magic+version+JournalID+最終USN+セクションテーブル+生列ダンプ+xxhash64)。temp→`MoveFileEx(REPLACE_EXISTING)`。起動時: ロード→検証→USN再生で追いつき→ライブ追従。失敗は常にフルリスキャンへ。

## FFI 契約(C ABI)

共通規約:
- DLL名 **`fmf_engine`**。全関数は `int32_t` ステータス(`FMF_OK=0`)+出力引数。
- 文字列は UTF-8(ファイル名は **WTF-8**: 不正サロゲートを保持。C#側は専用デコードでUTF-16へ復元)。
- ハンドルは opaque ポインタ。全関数スレッドセーフ。コールバック内からのFFI再入は禁止。
- 全入口で `catch_unwind` → `FMF_E_PANIC`。詳細メッセージは `fmf_last_error`(スレッドローカル)。

```c
// ── ライフサイクル ──
// config_json: { "index_dir": "...", "log_dir": "...", "log_level": "info" } (必須キー)
int32_t fmf_engine_create(const char* config_json, FmfEngineHandle* out);
int32_t fmf_engine_destroy(FmfEngineHandle h);          // 内部スレッドjoin+flush
int32_t fmf_flush(FmfEngineHandle h);                   // 明示保存

// ── イベント(エンジン内部スレッドから発火。受け側がDispatcherQueueへマーシャリング) ──
// kind: Progress(volume, scanned, total_estimate) / IndexChanged(エンジン側200msデバウンス、唯一のスロットル)
//       / RescanStarted(volume, reason) / VolumeFailed(volume, code)
typedef void (*FmfEventCb)(const FmfEvent* ev /*POD*/, void* user);
int32_t fmf_set_event_callback(FmfEngineHandle h, FmfEventCb cb, void* user); // cb=NULLで解除

// ── ボリュームと索引 ──
int32_t fmf_list_volumes(FmfEngineHandle h, FmfVolumeInfo* buf, uint32_t cap, uint32_t* count);
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
// 行: { entry_ref u64, frn u64, size u64, mtime i64, flags u32,
//       name_off u32, name_len u16, parent_path_off u32, parent_path_len u16 } + 末尾blob
// 戻り FMF_E_STALE = structural_generation 不一致。UIは同一クエリを再発行
int32_t fmf_result_page(FmfResultHandle r, uint64_t offset, uint32_t count, FmfPage** out);
int32_t fmf_page_free(FmfPage* p);
int32_t fmf_result_free(FmfResultHandle r);

// ── 診断 ──
int32_t fmf_last_error(char* buf, uint32_t* len);
int32_t fmf_engine_stats(FmfEngineHandle h, FmfStats* out);  // entries, heap_bytes, per-volume
```

エラーコード表(v2 pipeプロトコルと共用): `FMF_OK=0, FMF_E_INVALID_ARG=1, FMF_E_STALE=2, FMF_E_NOT_ADMIN=3, FMF_E_VOLUME=4, FMF_E_QUERY_SYNTAX=5, FMF_E_IO=6, FMF_E_PANIC=99`。

**MVPで意図的に入れないもの**: `fmf_entry_full_path`(行が name+parent_path を持つので不要)/ クエリキャンセル(クエリは数十ms想定。UIは世代カウンタで古い結果を捨てる。重くなったら `fmf_query_cancel` を追加する余地のみ残す)。

## C# 側の契約

- `IEngineClient`(差し替え境界): `Search(query, options) → ISearchResult(Count, GetRangeAsync)` / `event IndexChanged` / `event IndexProgress` / `ListVolumes` / `StartIndexing`。Fake/FFI/将来Pipeの3実装が同じ口に従う。
- `SearchResultHandle : SafeHandle`。ページフェッチは `DangerousAddRef/Release` を挟み、`Dispose()` 後も in-flight フェッチ完了まで実体を解放しない。
- ページ受領→`ResultRow` へコピー→**即 `fmf_page_free`**。
- コールバック delegate はクライアントのフィールドに保持(GC回収防止)。受領後 `DispatcherQueue.TryEnqueue` でUIへ。
- 再クエリの2系統: **タイプ起因=先頭リセット** / **IndexChanged起因=先頭可視インデックスと選択を退避→復元**。
- `VirtualResultList`(非ジェネリックIList+INCC+IItemsRangeInfo): indexer は絶対にフェッチせずプレースホルダ返却。`RangesChanged` で可視範囲±2ページを64行単位バックグラウンドフェッチ→既存 ResultRow のプロパティ充填(INCC Replace は発行しない)。ページLRU上限4096行。ハードSTALE受領→VMへ再クエリ要求。

## 遅延予算(変更→画面反映 ≤1s のAC内訳)

USNバッチ確定 ≤100ms + エンジンIndexChangedデバウンス 200ms(唯一のスロットル)+ UI再クエリ ≤100ms + 描画 ≤100ms = **≤500ms**(2倍余裕)。UI側に追加スロットルを置かないこと。
