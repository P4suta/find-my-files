# ADR-0023: 正規表現の第一級化(literal-prefilter 駆動+コンパイル上限、trigram は依然不採用)

日付: 2026-06-15 / 状態: 採用済み

## 決定

正規表現検索を隠し構文(`regex:` 手打ち)から第一級機能へ引き上げる。3点:

1. **全体モードの契約フラグ化**。`FmfQueryOptions` に `regex_mode:u32`(16→20B)を追加。bit0=クエリ全体を1本の regex として解釈、bit1=scope(0=名前/1=フルパス)、上位は予約0。UI は歯車メニューのトグル+「対象」サブメニューで切替。`regex:` 手打ちは従来どおり共存。クエリ書き換えで表現する案は却下(`|`/`!`/`"`/空白がパーサの AND/OR/NOT と二重解釈され、全体モードを安全に表現できない)。
2. **literal-prefilter 駆動**。regex を `regex_syntax` の prefix/suffix literal 抽出にかけ、必須リテラルを既存の fold 済みプール線形スイープ(`Driver::Sub`)に食わせて候補を絞り、regex 本体は residual で確認する。名前 scope のみ。抽出不能(`\d+`・先頭 `.*`・共通因子なしの選言)は full-scan + rayon。コンパイルは直前1件をエンジン内にキャッシュ(USN 再クエリ/RefreshInPlace の再コンパイル省略)。
3. **コンパイル上限**。`RegexBuilder` に `size_limit`/`dfa_size_limit`=各 1MiB。超過は `regex::Error::CompiledTooBig` → 既存 `CompileError::Regex` → `FMF_E_QUERY_SYNTAX(5)`。新エラーコードは追加しない。

非互換ワイヤ変更につき pipe 名を `fmf-engine-v1`→`v2`、`ABI_VERSION`/`PROTOCOL_VERSION` を 2 に上げる(規約「非互換変更は名前ごと上げる」)。

## 根拠

- **ADR-0002 との整合(最重要)**: prefilter は **trigram 転置索引ではない**。ADR-0002 が却下したのは「n-gram posting を常駐索引として維持する」こと(RAM +10〜15B/file、USN バッチ毎の差分維持)。本 prefilter は**クエリ compile 時に regex から literal を抽出して既存プールを線形スイープするだけ**で、常駐索引ゼロ・RAM 増分ゼロ・USN 差分維持ゼロ。「線形プールスイープ」という ADR-0002 の中核を regex にも適用するだけで、決定と矛盾しない。
- **線形時間保証**: rust `regex` crate は有限オートマトン(lazy-DFA/Pike VM、バックトラッキング無し)で**マッチ実行は入力長に線形 → ReDoS の実行時指数爆発は構造的に不在**(裏取り: docs/RESEARCH.md)。残る攻撃面は**コンパイル時間/メモリ**(巨大パターンの展開)。索引はファイル名のみ(p99 ≈110B)で正当な名前 regex は数十バイト級、1MiB プログラムには到達しない=正当ユーザを巻き込まず、悪性パターンを昇格サービス内でコンパイルさせず「丁寧に拒否」へ倒す。既定(10/2 MiB)より厳しく設定。
- **prefilter の正しさ**: prefix/suffix 抽出が返すのは「全マッチが先頭(末尾)に持つ literal」。その最長共通因子 `S` は全マッチに連続して存在 → 名前に存在。`S` を fold して fold 済みプールをスイープすれば case 両モードで superset(原文一致は fold 一致を含意・長さ保存)、regex residual が exact 確認。偽陰性ゼロが唯一不可分の正しさ要件で、oracle 差分テスト(prefilter == full-scan、name/path × case3)で担保。
- **v2 bump の理由**: 過去に「revert 後も古い protocol のサービスバイナリが稼働しクエリ破損」が起きた。16→20B は非互換で、pipe 名を上げれば古い v1 サービスには**到達できず**(20B 要求を 16B+text と誤読する事故が起きない)、Hello 版照合と二重に防ぐ。

## 実測(2026-06-15、実C: 160万件 / 合成1M criterion)

- **prefilter が効く regex は死守線内**: `regex:win.*\.dll`(接頭辞 "win")= p99 **9.1ms** @1.6M。micro でも `win.*\.dll` 8.5ms / `\.dll$`(接尾辞)7.1ms。既存クエリ(substring/wildcard/ext/size)は全て非退行。
- **literal-less regex は full-scan**: micro `regex:[0-9]{4}x` @1M = **28.7ms**(50ms 死守線内)。実C: `regex:[0-9]{6,}` は 160万件で p99 ~51ms — spec 規模(1M)では満たすが、件数に線形なので over-spec なボリュームで超える。memmem substring full-scan(`a`/`e` 単一文字)が 1.6M でも 6-8ms なのに対し、regex マッチは ~9倍重いため literal 無しのみが線形に伸びて 50ms を越える唯一のクラス。
- **ベンチ方針**: 実ボリュームの gated 集合(`P99_BUDGET_US=50ms` ハード)には prefilter で保証できる `regex:win.*\.dll` のみを置く。literal-less の最悪ケースは criterion micro(`query/regex_scan`、ungated)で計測・記録する — 固定 50ms 線で gate すると「マシンが spec より多くのファイルを持つ」だけで落ちるため(隠蔽ではなく、保証できる範囲のみを gate し最悪ケースは別途計測+本 ADR に明記)。
- pool 全体への regex ストリーミング最適化は `^`/`$` アンカー・エントリ境界跨ぎ・貪欲マッチで**不健全**なため不採用(literal 無し regex の full-scan は filename-only/no-index の受容済みトレードオフ)。

## 影響

- 契約進化(ADR-0018 フロー): ARCHITECTURE.md → `fmf-contract`(pod/options/versions)→ `FMF_BLESS=1` golden 再捕獲 → `just contract-gen` → 両言語テスト green。`contract/golden/query_req_*.bin` は 20B に再捕獲済み。
- `docs/SECURITY.md` 脅威 #5 に1行(regex コンパイル計算 DoS → 上限拒否)。
- キルスイッチ `FMF_REGEX_PREFILTER=0`(full-scan へ強制フォールバック。`FMF_QUERY_CACHE` と同型の現場回復弁)。
- 観測: prefilter 成功は `QueryTrace.driver` が `pool-scan`、抽出不能は `full-scan`。
- C# 側: `SearchOptions` に `RegexMode`/`Scope`、`PipeProtocol` の 20B 符号化、`AppSettings` 永続化、歯車メニュー UI、`RegexHighlighter`(.NET 再マッチ・ずれるなら光らせない)。

## 再検討トリガ

1. literal-less regex の full-scan が **@1M で 50ms 死守線を超える**(現状 ~29ms で内側。1M 規模で破ったら、prefilter 不能パターンのための別経路 — 例: pool ストリーミングの健全な部分集合や required-byte-class 事前フィルタ — を検討)。1.6M で 51ms なのは over-spec なボリュームによる線形増で、これ自体はトリガではない。
2. path scope regex の実需要が高く full-scan が p99 を破る → 名前部分アンカー抽出による path prefilter を検討。
3. 全体モードの典型クエリが literal-less に偏る実測 → そのとき初めて ADR-0002 の trigram 再評価(同 ADR の全トリガと AND)。
4. 正当ユーザが 1MiB コンパイル上限に当たる実報告。
