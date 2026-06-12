# ADR-0017: サービスのセキュリティモデル

日付: 2026-06-11 / 状態: 採用済み

## 決定

`fmf-service` は **LocalSystem** で実行し、install 時に `SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO`
で特権を最小集合に剥がす(SCM が宣言外特権をトークンから除去する — docs/RESEARCH.md)。pipe は
4層防御 — ①明示SDDL(SYSTEM+install時捕捉の利用者SIDのみ)②`PIPE_REJECT_REMOTE_CLIENTS`
③`FILE_FLAG_FIRST_PIPE_INSTANCE` ④接続時トークン照合(ImpersonateNamedPipeClient)— で
「同一ユーザーのみ・リモート拒否・匿名拒否」を保証する。`%ProgramData%\find-my-files` は install 時に
保護DACL(SYSTEM+Administrators、logs サブディレクトリのみ利用者 read)を設定する。
脅威モデルの常設文書は docs/SECURITY.md(本ADRは判断記録のみ)。

## 根拠

- **LocalSystem 採用 / 専用低権限アカウント+SeBackupPrivilege 却下**: 裏取りできた事実は
  「ボリュームハンドル(\\.\C:)のオープンは管理者必須」まで。SeBackupPrivilege が生ボリューム
  読みを通すという文書化された保証は存在しない(docs/RESEARCH.md — 通常ファイルのACLバイパスに
  ついての記述のみ)。未検証の権限構成に賭けず、検証済みの SYSTEM(Everything Service と同方式)+
  特権剥奪+ネットワーク機能ゼロ+pipe面の最小オペコードで攻撃面を絞る
- **利用者SID名指し / Authenticated Users 却下**: Authenticated Users RW はマルチユーザー機で
  他ユーザーが全ファイル名を検索できる(ACL迂回の名前漏洩 = Everything ETP 事件型)。
  **Administrators 許可も不成立**: UACフィルタ済みトークンでは Administrators SID が
  SE_GROUP_USE_FOR_DENY_ONLY になり、許可ACEに使われない(docs/RESEARCH.md)。よって install
  実行ユーザーの個別SIDを service.json に保存し、SDDL とトークン照合の両方で使う
- **OTS昇格(別の管理者アカウントで昇格)への対応**: 標準ユーザーが UAC で別の管理者資格情報を
  入力すると、install 実行ユーザー(=昇格に使った管理者)≠日常ユーザーになり、日常ユーザーが
  自分のサービスに接続できなくなる。非昇格UIが自分のSIDを `--owner-sid` で install に転送し、
  install は `validate_user_sid`(LookupAccountSid が SidTypeUser を返すもののみ採用)で検証して
  併記する。検証は脅威7(任意SID混入で他人が全ファイル名を読む)への防御 — install は昇格必須で
  既に sc.exe 相当の権限だが、解決不能/非ユーザー型のSIDは黙って捨てる(install 自体は落とさない)
- **`authorized_sids` の反映には再起動が要る**: サービスは起動時に一度だけ service.json を読み、
  DACL構築と接続時トークン照合の両方にその値を焼く(稼働中は不変)。よって稼働インスタンスへ
  SIDを追加するには `install`(冪等append)の後に `fmf-service restart`(stop→start)が必須 —
  `start` 単独は ERROR_SERVICE_ALREADY_RUNNING で no-op になり、古い許可リストで拒否し続ける
  (実機で `pipe client token rejected` 連発として現れた回帰の根因)。アプリの登録導線は
  install→restart を続けて実行する
- **多重防御の理由**: SDDL 文字列の構築ミスは「静かに全開放」になる事故パターン。構築関数を
  非昇格単体テストで構造ピンし、かつ接続受理時のトークン照合を独立に置く。匿名アクセスの遮断は
  明示DACL(匿名ACEなし=既定拒否)が一次防御 — NullSessionPipes の既定はマシン種別/ポリシー
  依存のため当てにしない(docs/RESEARCH.md)
- **%ProgramData% の保護DACL**: 既定ACLでは一般ユーザーが .fmfidx(全ファイル名を含む)を直接
  読める — pipe をどれだけ固めても脇から漏れる。logs だけ利用者 read を残す(非昇格 F12
  「診断情報をコピー」の動線維持)

## 影響

- install は SCM 登録に加えて SID捕捉→service.json、ディレクトリDACL、特権剥奪、
  `SERVICE_PRESHUTDOWN_INFO` の明示設定(現行Windowsの既定猶予は10秒しかない)を原子的に行う
  → sc.exe では表現できず `fmf-service install` サブコマンド一択(ロジックを単体テスト可能にする)
- uninstall は既定でデータを残す(`--purge-data` で .fmfidx/logs/service.json を削除)。
  残置物は README と SECURITY.md に明記
- **クライアント接続の前提(非昇格UIで実機検証)**: ①クライアントは pipe を Identification level
  で開く(C# `TokenImpersonationLevel.Identification` / Rust `SECURITY_SQOS_PRESENT`)— 既定の
  匿名レベルだとサーバの `ImpersonateNamedPipeClient` が匿名トークンになり、接続時SID照合が
  認可済みユーザーすら全拒否する。②クライアント側の偽サーバ検証(脅威4)は SYSTEMトークン照合
  ではなく SCM登録サービスのPID照合(`QueryServiceStatusEx`)で行う — 非昇格UIは SYSTEMプロセスの
  トークンを開けず(ACCESS_DENIED)、session 0 の identity も取れない。どちらも `authorized_sids`
  が空でトークン照合をスキップするコンソールモードのテストでは露呈せず、インストール済みサービス
  で初めて出る死角だった
- 「他ユーザー拒否」「リモート拒否」は開発機/CIで自動検証できない(別ユーザートークン・別マシンが
  必要)→ SDDL構築関数の構造ピン+SECURITY.md の手動チェックリストで代替。構築関数を経由しない
  pipe 作成コードパスを作らないこと(レビュー観点)
- 残存リスク(受容): 認可済みユーザーは自分のACLで見えないファイルの「名前・パス」も検索できる
  (ファイル名のみ索引の構造的性質。内容は読めない)。SECURITY.md に明記

## 再検討トリガ

- 低権限インデクサの実証が文書化された場合(例: FSCTL_READ_UNPRIVILEGED_USN_JOURNAL 相当の
  生ボリューム読み手段)→ LocalSystem の降格を再評価
- SERVICE_SID_TYPE_RESTRICTED+index ディレクトリへの明示ACE(v2.1 の硬化候補)
- マルチユーザー機の実需要(認可SID複数登録の UX)
