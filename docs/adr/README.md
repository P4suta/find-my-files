# ADR索引

- [0001](0001-filename-only-index.md) — ファイル名のみ索引(content/プロパティ索引はしない)
- [0002](0002-linear-sweep-no-trigram.md) — 線形プールスイープ+インクリメンタル検索、trigram転置索引は不採用
- [0003](0003-wtf8-length-preserving-fold.md) — 名前はWTF-8格納、foldは長さ保存の単文字小文字化のみ
- [0004](0004-fold-overflow-name-layout.md) — fold済みプール1本+原文オーバーフロー(−16B/entry)
- [0005](0005-frn-index-sorted-permutation.md) — FRN索引はソート済みid順列(25→12→4B/entry)
- [0006](0006-lazy-sort-permutations.md) — size/mtime順列は遅延derived cache(−8B/entry)
- [0007](0007-size-u32-overflow.md) — size列u32+オーバーフローmap(−4B/entry)
- [0008](0008-insertion-point-batch-merge.md) — USNバッチは挿入位置マージ(54.6→2.0ms@1M)
- [0009](0009-compaction-order-preserving-remap.md) — コンパクションは旧id昇順リマップ・再ソートなし
- [0010](0010-snapshot-raw-pod-no-compat.md) — スナップショットは生POD+全検証・後方互換なし
- [0011](0011-scan-streaming-pipeline.md) — ストリーミングスキャン採用、I/O多重化は却下(+14.4% < +30%)
- [0012](0012-default-allocator-record-arena.md) — 既定アロケータ+RecordArena、mimalloc却下(WS +260MB)
- [0013](0013-measurement-discipline.md) — 計測規律: 冷機・back-to-back・実ボリューム絶対ゲート
- [0014](0014-build-tooling-rejections.md) — rust-lld/sccache/nextest却下、codegen-units=1の理由
- [0015](0015-winui-data-virtualization.md) — WinUI 3データ仮想化(IList+INCC+IItemsRangeInfo)
- [0016](0016-service-split-named-pipe.md) — v2サービス分離: fmf-service+named pipe、トランスポート却下案、flush公開面
- [0017](0017-service-security-model.md) — サービスのセキュリティモデル: LocalSystem+特権最小化、pipe DACL 4層
- [0018](0018-contract-single-source.md) — 契約の単一正本化(fmf-contract)+捕獲先行ゴールデンコーパス、シーム2本上限
