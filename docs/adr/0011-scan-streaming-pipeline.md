# ADR-0011: ストリーミングスキャンパイプライン(I/O多重化は却下)

日付: 2026-06-11 / 状態: 採用済み

## 決定

初回スキャンは$MFTを16MiBチャンクのbuffered同期ストリーミング読みとし、専用I/Oスレッド1本がチャンクN+1を先読み(バッファ3本でRAM上限固定)、チャンク内はレコード境界1MiBサブレンジでrayon並列パースする。$ATTRIBUTE_LIST(deferred)の名前解決は、ストリーミング中に$FILE_NAME持ち拡張レコードをRAMキャッシュ(上限128Ki件≈128MiB一時)し、ディスク読みゼロで行う。NO_BUFFERING+overlappedによるI/O多重化は採用しない。

## 根拠

- deferredパス実測: ディスク読み版2.9s → RAMキャッシュで**8ms**。`\\.\C:` のランダムリードは発行ハンドル数によらずカーネルで直列化されるため、並列I/Oでは縮まない
- スキャン全体 5.0s→2.1s(read 1.6sが律速)
- `fmf io-probe C:`($MFT 1.54GiB)実測: buffered同期 962.6 / +SEQUENTIAL_SCAN 960.9 / NO_BUFFERING同期 958.2 / NO_BUFFERING+overlapped QD2 1075.9 / QD4 **1101.6 MB/s(+14.4%)**。採用基準(read +30%でStage 2、scan全体−25%で採用)未達。scan全体への効果は2.0→~1.85s見込みで、M2スキャンゲート(60s)を30倍クリア済みの現状に見合わない
- EntryId割当はワーカーバッチをチャンク順に追記するため逐次版と決定的に一致(adminテスト `streaming_scan_matches_reference` が等価性ゲート)

## 影響

- 一時RAM: パイプラインバッファ3×16MiB+拡張レコードキャッシュ(上限128Ki件)。超過は `ext_name_cache_skipped` カウンタ+ディスクフォールバック
- I/Oスレッド起動失敗は `scan_pipeline_fallbacks` カウンタ+逐次読みに劣化(黙らない)
- `fmf io-probe` は計測ツールとして常備する

## 再検討トリガ

- マルチボリューム同時スキャン要件が立った場合、またはスキャンゲートが10s以下に強化された場合(overlapped多重化のStage 2を再評価)
