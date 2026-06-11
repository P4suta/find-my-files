# ADR-0004: fold-overflow名前レイアウト

日付: 2026-06-11 / 状態: 採用済み

## 決定

連続スイープ可能な全長プールはfold済み `lower_pool` の1本だけとする。原文はfoldと異なる場合のみ `orig_pool` に格納し、`orig_off`(u32、sentinel `u32::MAX` = fold同一)で参照する。`name()` はfold同一エントリでlowerスライスをそのまま貸す。

## 根拠

- 実C:実測(1,268,450件): fold同一(lower == orig バイト一致)= 73.2%。名前の二重格納の約3/4は同一バイトの重複
- 実測 −16B/entry。M2 RAMゲート(≤110B/entry)達成の最大単項
- 健全性3本柱: (1) foldは長さ保存(ADR-0003) (2) 原文一致 ⇒ fold一致(supersetスイープが健全。subsume.rs の bridge_needle と同じ代数) (3) fold不安定needle(自身のfoldと異なるneedle)はfold同一名に出現不可 → `orig_off` sentinel一発のO(1)棄却で候補の73%を解決
- 対案(i)「(entry, orig_off) ソート済みペア」は予測 −19.6B/entry でu32カラム方式(−17.7B)より1.9B優位だが、residual検証毎の二分探索が乗るためp99リスク回避でu32カラムを採用

## 影響

- 大文字を含むneedle(smart case)とSensitiveモードは原文を直接スイープできず、foldしたneedleのスーパーセットスイープ+原文residual検証になる
- 受け入れた劣化: 実C: 大文字needle("Win"、smart-case)p50 2.5→3.6ms(p99予算50msの7%)
- スナップショットも同分だけ縮小(FMFIDX04)

## 再検討トリガ

- 実ボリュームでfold同一比率が大きく崩れる名前分布(50%未満級)が観測された場合
