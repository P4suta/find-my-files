# ADR-0005: FRN索引はソート済みid順列

日付: 2026-06-11 / 状態: 採用済み

## 決定

FRN→EntryId索引はソート済みid順列(ids u32 = 4B/entry、index/frn.rs)のみで持つ。比較キーはfrnカラムへの間接参照で読む。lookupは未マージ尾部をnewest-firstに走査(バッチ内の最新upsertが勝つ)→本体を二分探索し、常にtombstone生存フィルタを通す。

## 根拠

- FxHashMap実装は ~25B/entry(16バイトslot+バケット容量パディング+制御バイト。実C: frn行 31.2MB)で名前プールに次ぐRAM最大項
- keys u64 + ids u32 の2配列化で 12B/entry(frn行 31.2→15.1MB、WS 157→140B/entry。M0ゲート ≤150B を初通過)
- keysは masked(frn[ids[i]]) の純冗長コピー → 削除して 4B/entry(−8B/entry、実C:で約10MB)
- lookupが乗るのはUSN適用経路とビルダのparent解決のみで、検索ホットパスは触らない。間接参照による+1キャッシュミスは許容
- 副産物: 復元が直列hashmap insert百万回 → 並列ソート1回になり、criterion load_1m 89.4→58.9ms(−34%)

## 影響

- 削除はtombstoneのみでunmap不要。rename/NTFSレコード再利用は同キーの死重複を残すが、生存フィルタ下で生存は常に高々1(ランダムrename/削除ストームのforward-merge参照とのバイト同一テストで固定)
- 初回スキャンのビルダはparent解決を遅延し finish() の並列パスで一括解決(未マージ1M尾部への都度lookupはO(n²)のため)。build_ms 13→64ms、read律速2.1sのスキャン内で不可視

## 再検討トリガ

- 検索ホットパスがFRN lookupを要する設計変更が入った場合
