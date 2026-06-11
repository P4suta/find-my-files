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

- **VolumeIndex(ボリューム毎、struct-of-arrays)**: 名前は**fold-overflow レイアウト**(index/core.rs) — 連続スイープ可能なプールは fold 済み `lower_pool` の1本だけで、原文は fold と異なる場合(実C:実測 26.8%、docs/RESEARCH.md)のみ `orig_pool` に格納し `orig_off`(u32、`u32::MAX`=fold同一)で参照。`name()` は fold 同一エントリでは lower スライスをそのまま貸す(バイト同一)。name_off(u32)+name_len(u16)(fold は長さ保存、wtf8.rs)/ parent(EntryId=u32)/ **size は u32 カラム+オーバーフロー map**(4GiB 超は実C:で10件 — sentinel `u32::MAX`)/ mtime(i64, FILETIME)/ frn(u64)/ flags(is_dir|tombstone|reparse|hidden|system|excluded)/ **FRN→EntryId はソート済み id 順列のみ**(index/frn.rs: ids u32=4B/entry。キーは frn カラムを間接参照 — 検索ホットパスは触らないので+1ミスで済む)/ fast sort 順列は **name の1本のみ常時維持**。size/mtime 順は derived cache の遅延順列(query/memo.rs の SizePerm/MtimePerm: 初回ソートクエリで par_sort 一回、以後世代毎に差分マージ、非永続)。パス文字列は保持せず親チェーンで遅延構築。削除は tombstone、閾値超でコンパクション(下記)。
- **USNバッチのマージは挿入位置二分探索+in-placeセグメント移動**(index/mod.rs `merge_sorted_tail`): バッチ(~1k)対既存(~1M)で全長要素比較・全長再アロケをやめ、O(batch·log n) 比較+copy_within 一回。実測 54.6ms→2.0ms/バッチ@1M。容量は `reserve_exact(max(add, len/64))` でスラック上限~1.6%。
- **コンパクション(実装済み)**: volume スレッドがバッチ適用毎に判定(`len≥100k かつ tombstone_ratio>12.5% または dead_name_bytes>32MiB`)。**旧id昇順リマップで perm/FRN索引はソートなしの filter+remap**(O(n) コピー、生存エントリの相対順序が保存されるため)。read guard 下でコピー構築(クエリ並走可、書き手は volume スレッド1本)→ `install_index` で µs 級 swap+structural bump → 開いている結果ハンドルはハードSTALE→UI再クエリ。死んだ dir の子は root へ付け替え(push_raw の orphan 方針と同じ)。防御の世代チェック失敗は `compaction_aborts` カウンタ+破棄。
- **FRN索引のlookup意味論**: 検索順=未マージ尾部(バッチ内の最新upsertが勝つ)→二分探索、**常にtombstone生存フィルタ**(削除=tombstoneのみ、unmap不要。rename/レコード再利用は同キー複数ペアになるが生存は常に高々1)。バッチ末尾の `merge_new_into_permutations` がperm群と同じ2-pointerマージで尾部を畳む。初回スキャン中はビルダがparent解決を遅延(全部finish()の並列パスで解決)するためO(n²)にならない。
- **既定除外(EXCLUDED)**: flagsにH(hidden)/S(system)の生属性と、計算済みEXCLUDEDビット(自分がH|S、または祖先にH|Sがいる)を保持。クエリは既定でEXCLUDEDをスキップ(`include_hidden_system`で解除)。継承はスキャンfinish時にO(n)伝播、USN挿入/移動時に親から再計算。**制限**: 除外ブランチからのsubtree移動では配下のビットが次回リスキャンまで陳腐化(dir-rename制限と同類)。
- **generation 2層**:
  - `content_generation`: USNバッチ適用毎に++。既存結果ハンドルは**読み出し継続可**(tombstone+末尾追記なので安全。削除済みが残る/新規が出ないだけの Everything 同等の結果整合)。
  - `structural_generation`: コンパクション/フルリスキャン時のみ++。既存ハンドルは**ハードSTALE**(`FMF_E_STALE`)。実装: index の差し替えは必ず `VolumeSlot::install_index` を通り、旧 index の値+1 を新 index に引き継ぐ(初回インストール/スナップショット復元は bump しない)。スナップショットはこの値を**永続化しない**(復元時0)— 結果ハンドルはプロセスを跨がないため、プロセス内の単調性で十分。
- **クエリ時マテリアライズ**: `fmf_query` がボリューム毎の該当順列を1パスフィルタし、ソート順確定済みの連続配列 `Vec<u64>`(volume_id<<32|entry_id 相当)に確定+マルチボリューム k-way マージ(単一ボリュームは直コピーのfast path)。以後のページ取得は **O(1) スライス**。列クリック=同一クエリを別ソートで再発行。
- **インクリメンタル検索(クエリキャッシュ)**: `VolumeSlot::last_query` がボリューム毎に直前の(compiled, options, 両generation, ids)を保持。新クエリが前回を**証明可能に**絞り込み(`query/subsume.rs`の保守的包含規則: 同一ソート・単一ANDグループ・needle包含/範囲縮小/フィルタ追加のみ。fold橋渡しはorig→folded方向のみ健全)、かつ両generationが不変なら、`query::refine` が前回idsを完全評価でフィルタ — **O(前回ヒット数)**でスキャンもO(n)マテリアライズも省略(Everything本家の核心技法)。USNバッチはgeneration++で暗黙に無効化、構造交代は`install_index`で明示クリア。正しさはoracleプロパティテスト(refine==fresh search)で担保、キルスイッチ=`FMF_QUERY_CACHE=0`、観測=`QueryTrace.cache`(miss/refine/partial)。FFI/C#は完全無変更。
- **ロック**: `parking_lot::RwLock`。検索=read、USNバッチ適用=write(ms級)。
- **スレッド**: 初回スキャン=ボリューム毎1スレッド並列。USN追従=ボリューム毎1スレッド(ブロッキング読み→吸い尽くし→バッチ適用)。停止は `CancelSynchronousIo`。
- **初回スキャンの内部並列**: $MFTを16MiBチャンクでストリーミング読みしつつ、(a) 専用I/OスレッドがチャンクN+1を先読み(read-aheadパイプライン、バッファ3本で上限RAM固定。スレッド起動失敗は`scan_pipeline_fallbacks`カウンタ+逐次読みに劣化)、(b) チャンク内はレコード境界1MiBサブレンジでrayon並列パース(fixup→属性抽出→WTF-8変換までワーカー側)。ワーカーバッチをチャンク順に追記するためEntryId割当は逐次版と決定的に一致(等価性ゲート=admin test `streaming_scan_matches_reference`)。
- **deferred($ATTRIBUTE_LIST)名前解決はRAMから**: ターゲットは必ず拡張レコードで、ストリーミング中に全$MFTが手元を通る — $FILE_NAME持ち拡張レコードをパース時にキャッシュ(上限128Ki件≈128MiB一時、超過分は`ext_name_cache_skipped`+ディスクフォールバック)し、deferredパスをディスク読みゼロのCPU処理にする。実C:(20k件)実測: ディスク読み版2.9s(ボリューム直読みはハンドル数によらずカーネルで直列化されるため並列I/O不可)→ **8ms**。スキャン全体5.0s→2.1s(read 1.6sが律速)。
- **検索実行**: クエリ→AST→`CompiledTerm` 列(コスト順: 数値フィルタ→memmem→wildcard/regex、AND短絡)。rayonで64kチャンク並列。**スイープは常に lower_pool**(唯一の連続プール)。smart case で大文字を含む needle / Sensitive モードは **fold した needle のスーパーセットスイープ+原文 residual 検証**(原文一致⇒fold一致の含意。subsume.rs の bridge_needle と同じ代数)。residual には O(1) fast path: fold 不安定文字を含む needle は fold 同一名(73%)に絶対に現れない(`CTerm::exact_needle_unstable` + `orig_off` sentinel 一発)。`dm:` はローカルTZ解釈。NFC/NFD正規化はしない(既知制約)。
- **派生キャッシュ(OffsetTable/DirPaths)**: content_generation毎に世代管理。OffsetTableはプール追記が常に末尾である性質を使い、世代遷移で**ソートなしの差分延長**(前世代テーブルを可能ならin-place再利用+追記分マージ。in-place dirリネームの旧ペアはstaleとして残し、sweepが`name_off`照合でスキップ)。stale比がn/8超で正常な方針切替としてフル再構築。ウォーターマーク不整合(起きないはずの状態)は warn+`offset_table_rebuild_fallbacks`+フル再構築。DirPathsはprewarmせず初回pathクエリで遅延構築(pathクエリを使わないセッションのRAMをディレクトリ全パス×2プール分節約)し、以後は**dir-topology世代**(`rename_dir_in_place`/dirの`reparent`のみが++)が不変な限り追記分のみの差分延長(合成1M実測: USNバッチ後の初回pathクエリ21ms→5ms)。dirリネーム/移動はフル再構築(正常な方針切替)。両キャッシュのバイト数は `IndexStats.derived_cache_bytes` としてB/entryゲートの分母に計上。
- **n-gram(trigram)索引は不採用(2026-06判定・数値根拠つき)**: 合成1M件でのコールド3文字クエリは約2.9ms(クエリキャッシュMISS+派生キャッシュwarm、materialize込み)で、判定基準「per-volume scan_us p99 > 25ms @1M」を一桁下回る。線形プールスイープ(SIMD memmem)+インクリメンタル検索+OffsetTable差分延長で予算内のため、posting維持コスト(RAM ≤110B/file制約下で+10〜15B/file、USNバッチ毎の差分維持)に見合わない。**再検討トリガ**(全て満たす場合のみ): (1)キャッシュMISS時コールド3文字 scan_us p99>25ms@1M (2)`fmf stats --trigram-estimate` の実測見積もり≤15B/fileかつ合計≤110B/file (3)posting差分維持≤2ms/バッチ (4)1ボリューム400万件超の実需要。
- **永続化**: `{index_dir}\{volume-guid}.fmfidx`。自前バイナリ **FMFIDX04**(magic+JournalID+最終USN+生列ダンプ+xxhash64。セクション: lower_pool / orig_pool / orig_off / name_off / name_len / parent / size_lo / size-overflow ids+sizes / mtime / frn / flag / perm_name)。ロード時は checksum に加え **全スライス境界とオーバーフロー対応の構造検証**(壊れた入力で panic せず Err→リスキャン)。size/mtime 順列と FRN 索引は非永続(初回利用時/復元時に再構築)。temp→`MoveFileEx(REPLACE_EXISTING)`。起動時: ロード→検証→USN再生で追いつき→ライブ追従。失敗は常にフルリスキャンへ。実C: 1.27M件で 92.4MiB。

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
- **劣化カウンタ**(`MetricsSnapshot.counters`、0でなければF12に表示): stat_fetch_failures / usn_batches_truncated / snapshot_load_failures / snapshot_save_failures / deferred_names_unresolved / corrupt_mft_records / journal_rescans / scan_pipeline_fallbacks(スキャンのread-ahead I/Oスレッド起動失敗→逐次読みに劣化)/ offset_table_rebuild_fallbacks(オフセットテーブルのウォーターマーク不整合→フル再構築に劣化)/ lazy_perm_rebuild_fallbacks(遅延ソート順列の同種防御)/ compaction_aborts(コンパクション中の世代不整合→コピー破棄。単一書き手不変条件の破れ検知)
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
