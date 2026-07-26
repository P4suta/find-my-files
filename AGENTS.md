# find-my-files

Windows専用の爆速ファイル名検索。Rustエンジン(`engine/`)+ WinUI 3 UI(`app/`)。Apache-2.0。

## やらないことリスト(スコープクリープ防止。最重要)

content検索 / プロパティ・タグ索引 / プレビュー / FTP・HTTP・ETPサーバ / FAT・exFAT・ネットワークドライブ(MVP) / ReFS(MVP) / クロスプラットフォーム化。
「ファイル名だけを索引する」割り切りが速度の源泉(content索引はRAMが桁違いに膨らむ)。

## アーキテクチャ固定則

- 依存方向: `app → IEngineClient → (PipeEngineClient → named pipe → fmf-service | FfiEngineClient → fmf_engine.dll (fmf-ffi)) → fmf-core`。既定は Pipe(非特権UI)、`--engine=inproc` が昇格in-procフォールバック
- **DLL名 = `fmf_engine`。変更禁止**(`fmf-ffi/Cargo.toml` の `[lib] name` と C# の `[LibraryImport("fmf_engine")]` が対)
- **fmf-ffi にロジックを書かない**(変換・ハンドル管理・catch_unwindのみ)。fmf-service も同じ理由で dispatch=写像のみ(ロジックは fmf-core)
- app は `Engine/IEngineClient.cs` 経由でのみエンジンに触る(Fake/Ffi/Pipe の3実装が同じ口)
- 契約の散文正本は `docs/ARCHITECTURE.md`(エラー表は追記のみ・再番号禁止)、**機械可読正本は `fmf-contract`**(依存ゼロのleafクレート・定数/repr型/レイアウト/純バイト変換のみ・ロジック禁止)。codecは `fmf-proto`。変更フローは一方向放射: ARCHITECTURE.md → fmf-contract → `FMF_BLESS=1` 再捕獲 → `just contract-gen` → 両言語テストgreen(ADR-0018)
- Rust/C# 両テストが `contract/golden/` の同一捕獲バイトをピンする(再捕獲は `FMF_BLESS=1` の明示儀式のみ)。`app/FindMyFiles/Engine/Generated/` は**手編集禁止**(`just contract-gen` で再生成。漂流は `just test` の drift が検出)
- **エンジン内の trait シームは `engine/seams.rs` の SnapshotStore / JournalSource の2本が上限。追加ポート化禁止**(ADR-0018。再検討トリガ: admin専用経路の実退行)
- インデックス置き場: `%ProgramData%\find-my-files\`(マシン単位。`.writer.lock` で単一書き手をプロセス間強制 = FMF_E_LOCKED)。サービス設定: 同所 `service.json`(サービス所有)。UI設定: `%APPDATA%\find-my-files\settings.json`
- **サービスはオンデマンド(ADR-0027)**: `DEMAND_START`(boot常駐しない)。installで安定exeを `%ProgramData%\find-my-files\fmf-service.exe` にコピー+サービスObject DACLでユーザーにstart/stop権付与(change-config/delete禁止=LPE防止)。アプリ起動時に非昇格start→pipe接続。`serve()` は `idle_stop_secs`(既定300秒・0で従来常駐)無接続で自己停止。日次SYSTEMタスク `fmf-service gc` が `last_use` を見て `gc_max_idle_days`(既定7・0で無効)放置なら登録ごと削除。契約/golden不変
- **ビルド出力は repo直下 `build/` の単一ツリーに集約**(ADR-0021): `build/{engine,xtask}`=cargo target-dir(各 `.cargo/config.toml`)、`build/app`=C# bin(csprojの`BaseOutputPath`)、`build/dist/FindMyFiles`=publishバンドル(ルートに `BUILDINFO.txt`=版/チャネル/commit/date 同梱・ADR-0038)、`build/package`=zip+SHA256SUMS(coreutils形式・`sha256sum -c` 可)、`build/sbom`、`build/site`+`build/docs-book`=docs。出力パスの正本は `xtask/src/paths.rs`。**C# obj だけは `app/**/obj/` に据え置き**(移設は禁止の `Directory.Build.props` が必要)。`.gitignore` は `build/` 1行で全成果物を除外
- pipe のセキュリティは `docs/SECURITY.md` が正本。**SDDL構築は fmf-service/src/security.rs の構築関数経由のみ**(直書き禁止)

## 開発環境(このマシンは chezmoi + mise 管理)

- ツールチェーンは `mise.toml` でピン留め(rust/dotnet)。**rustup・winget等でのアドホックなツール導入はしない**。新ツールが要るなら `mise.toml` に追記して `mise install`
- タスクランナーは `just`(`justfile`参照): `just build` / `just check`(型チェックのみ・日常ループ)/ `just test` / `just test-app`(C#)/ `just test-admin`(昇格)/ `just test-pipe`(C#×実サービス結合・FMF_PIPE_TESTS)/ `just lint` / `just fmt` / `just contract-gen`(EngineContract.g.cs再生成)/ `just clean-temp`(test-tmp掃除)/ `just index C:` / `just bench` / `just bench-check`(20%回帰ゲート)/ `just bench-baseline`(基準再記録)/ `just profile`(samply・昇格)/ `just perf-gate`(実ボリューム+microの両回帰ゲート・昇格)/ `just service-dev`(コンソールサービス・昇格・開発内ループ)/ `just service-install|start|stop|restart|status`
- **版/タグ/CHANGELOG は release-please が正本(ADR-0035)**: Conventional Commits → Release PR を自動維持 → マージで `vX.Y.Z` タグ。**手動の `xtask release` / `just release` は廃止**(人は版番号を選ばない)。release-please は `release-please-config.json`+`.release-please-manifest.json`、`RELEASE_PLEASE_TOKEN`(GitHub App/PAT)で点火する休眠配管(GITHUB_TOKEN のタグは release.yml を起こさない再帰防止のため)。dev/nightly/stable の刻印は `engine/crates/fmf-buildstamp`(build.rs・**core/ffi 非依存の終端**)+ C# `InformationalVersion`、文字列形式の正本は `xtask version --channel`。`fmf --version` は `fmf_buildstamp::VERSION`
- ビルド/配布の手続き的ロジック(`publish` / `publish-app` / `package` / `clean-temp` / `version`)は **`xtask/` クレート**に集約(cargo-xtask パターン)。justは薄いラッパーで `cargo run --manifest-path xtask/Cargo.toml -- <cmd>` を呼ぶだけ。**xtask は repo直下の独立ワークスペース。engine ワークスペースのメンバーにしない**(`cargo *--workspace*` の日常ループ/`llvm-cov --fail-under-lines` に混入するため)。純ロジック(チャネル版整形=version.rs / locale剪定 / checksum / semver)は xtask 内でユニットテスト。インライン PowerShell をここに戻さない。CIゲートは ubuntu `xtask` ジョブ(test+clippy+fmt)+ advisory は cargo-audit.yml(engine と並列で xtask/Cargo.lock も)
- 最適化セッションは `just bench-micro-baseline` で開始し、変更毎に `just bench-micro-check`(criterion 10%ゲート・非昇格)。fmf-core を触ったらマージ前に昇格シェルで `just perf-gate`
- git hookは `lefthook`(`just setup` で導入)。pre-commit: typos+rustfmt+taplo、pre-push: clippy+test+test-app
- `rust-toolchain.toml` / `global.json` は意図的に置かない(miseと二重管理になるため)

## シェルの既定(bash/PowerShellで迷わない)

このマシンは複数シェル(PowerShell=primary / Git Bash=POSIX)が同居する。**毎回どちらか迷う**のを避けるため既定を固定する。「どのシェルで走るか」を定義が気にしなくて済む状態が正(CIは既にYAML `env:` で達成済み)。

- dev/ビルドの作業は **`just <recipe>` が唯一の入口**(`justfile`がシェルを吸収)。生の cargo/dotnet/git を直叩きしない。新しい定常タスクはレシピ化する
- ad-hocなワンショットは **PowerShell**(primary)を既定に書く。`NUL` / `$env:VAR` / バッククォート継続。`/dev/null`・forward-slash・`export VAR=` 前提で書かない。Git Bash(POSIX sh)は**真にPOSIXが要る時だけ**(heredoc・パイプ前提のワンライナー等)— lefthookが内部でshを使うのも同じ割り切り
- **justレシピ/lefthookフックにシェル固有構文を書かない**(`set windows-shell := powershell.exe` は起動インタプリタの実装詳細にすぎず、定義の正しさをシェルに依存させない): env設定は cargo `--config 'env.X="1"'` / dotnet `.runsettings`(`app/FindMyFiles.Tests/pipe.runsettings`)、複数コマンドは `&&` でなくレシピの複数行/ジョブ分割で表す
- 例外: `release.yml` の署名ステップ(`Get-AuthenticodeSignature` 等)はWindows OS固有APIに正当に束縛され、`pwsh` 明示でよい

## 昇格(管理者権限)の規約

- MFT読み・USNジャーナル読みは**昇格必須** → 担うのは fmf-engine サービス(または昇格ターミナルからの `--engine=inproc` / `just service-dev` / 実ボリュームテスト・ベンチ)。**初回installのみ昇格(UAC1回)**。以後のstart/stopはサービスObject DACLでユーザーに付与済みなので**非昇格**(ADR-0027)
- アプリ(asInvoker)とUIテスト(winapp ui)は**非昇格でよい**。決定的データは `--fake-engine` 起動
- `just build` / `just test`(nextest。単体・pipeループバック含む) / `just lint` / `just fmt` は非昇格でOK(USNロジックはfixtureリプレイでテストする設計)
- 昇格必須テストは `#[ignore]` + 環境変数 `FMF_ADMIN_TESTS=1` でゲート(サービスE2E含む)。昇格シェルで `just test-admin`
- サービス稼働中に `--engine=inproc` を起動すると FMF_E_LOCKED(単一書き手)— `just service-stop` してから

## UI固定則(WinUI 3の罠。違反するとリグレッション)

- ListViewの `ItemsPanel` は `ItemsStackPanel` から変えない(変えると仮想化が死ぬ)
- **ItemsSource の差し替え禁止**: `VirtualResultList` はページと同寿命の単一インスタンス(x:Bind OneTime)。新しい結果は `Reassign`(プリフェッチ済みseed+Reset)で公開する(差し替えるとListViewの仮想化状態が破棄され、ちらつきが再発する)。ただしエンジンが `QueryTrace.unchanged=true`(同一クエリ・同一ID列)を返した再クエリは **Resetを発行せず `RefreshInPlace`**(ハンドル差し替え+可視行のin-place充填)— アイドル時のUSN起因再クエリで画面がちかちかしないための非対称
- `ISupportIncrementalLoading` 禁止(クラッシュ報告 microsoft-ui-xaml#6883)。データ仮想化は非ジェネリック `IList` + `INotifyCollectionChanged` + `IItemsRangeInfo`(IList<T>だけではダメ、#1809)
- ItemsView / ItemsRepeater は使わない(上記インターフェース非対応)
- `x:Bind` + `x:Phase`、ブラシは `ThemeResource` のみ(色のハードコード禁止)
- `DispatcherQueue.GetForCurrentThread()` はUIスレッドでキャッシュし、バックグラウンドからは `TryEnqueue`
- FFIコールバックに渡すdelegateはフィールド保持(GC回収→ネイティブ側ダングリング防止)
- 「開く」は `explorer.exe "<path>"` 経由で脱昇格(直接openすると関連付けアプリが管理者で起動する)
- **エンジン/トランスポート変更(onboarding・サービス管理)は in-process ソフト再起動(`App.SoftRestart*`/`AppReload`=エンジン再解決+ページ再構築)で行う。プロセス再起動(`Process.Start`+`Exit`)を戻さない**(単一インスタンス化ADR-0030がリダイレクトで握り潰しアプリが消える=#107バグ・ADR-0036)。真のプロセス再起動が要るのは言語変更だけで、単一インスタンス安全な `AppInstance.Restart` を使う
- 独自 `Directory.Build.props` を作らない(BuildAndRun.ps1のアナライザ注入が黙ってスキップされる)

## 性能合格ライン(リリース前に `just bench` で確認)

| 指標 | M0 | 最終(M2) |
|---|---|---|
| 初回インデックス(実C:) | 25万≤8s | 25万≈5s / 100万≈60s |
| 検索 p99(100万件、3文字以上) | ≤50ms | ≤50ms |
| RAM(エンジン単体、バイト/ファイル) | ≤150B | ≤110B |
| 変更反映(USN→UI) | — | ≤1s(エンジン側デバウンス200msの1箇所のみ) |
| スナップショット復元→ready | — | ≤2s |

RAM測定はエンジンプロセス(fmf-cli)単体のWorkingSet。アプリ全体WSはWinUI/.NETベースラインが乗るため別枠の参考値。

## エラーハンドリング規約(原則: 落ちない・固まらない・黙らない)

- ログ: エンジン=`%ProgramData%\find-my-files\logs\engine.log`(`FMF_LOG`でフィルタ)、アプリ=`%APPDATA%\find-my-files\logs\app.log`。異常調査はまずここ+F12パネル
- C#のfire-and-forgetは**必ず** `task.Forget("area")`(`_ = SomeAsync()` 禁止)。シェル操作は`ShellOps`経由
- Rustの劣化パス(フォールバックで回復するもの)は**必ず** `fmf_core::degrade!`(warn+カウンタを不可分に行う唯一の手段。`rg degrade!`=劣化パス全列挙)。例外: スキャン内部のバッチ経路は劣化を `ScanStats` フィールドで返し、worker層の1箇所で counters+warn へ写像(ホットパスにマクロを散らさない)。境界クレート(fmf-ffi/fmf-service)は clippy.toml の disallowed-methods が `unwrap_or_default` を禁止
- カウンタ追加は3点セット: `metrics.rs`(Counters+CountersSnapshot)+ `fmf-contract::counters::COUNTER_NAMES` + `just contract-gen`(C#のCountersDataは生成)。漂流は golden テストが検出
- 故障注入: DEBUG+`--fake-engine` はクエリ `!!warn` / `!!panic` / `!!lag`。pipe経路は `fmf-service run --debug-faults` で `!!panic`(接続生存のままFMF_E_PANIC)/ `!!drop`(強制切断→再接続経路)/ `!!lag`(ページ250ms遅延)— インストール済みサービスでは常に無効

## 参照ドキュメント

- `docs/ARCHITECTURE.md` — FFI正本契約・スレッドモデル・generation 2層・遅延予算
- `docs/adr/` — 設計判断・却下判断の記録(数値根拠・再検討トリガつき)。**構造を変える前に該当ADRを読む**
- `docs/RESEARCH.md` — 裏取り済み外部事実(MFT/USN API仕様、Everything実測値、出典付き)。**設計判断の前に必ず読む**
