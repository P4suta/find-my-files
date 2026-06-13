# ADR-0020: コード署名プロバイダ選定(SSL.com eSigner / 個人IV)

日付: 2026-06-13 / 状態: 採用(配線は休眠 — 証明書未取得。手順書 docs/SIGNING.md)

## 決定

配布バイナリの Authenticode 署名は **SSL.com eSigner**(クラウド HSM 署名サービス)+ **個人 Individual Validation(IV)
証明書**で行う。署名は `release.yml`(タグ駆動)の **CI 環境固有 YAML ステップ**として保持し、`xtask/` には入れない。
証明書取得までは **非ブロッキングで休眠**(Secrets 未設定なら未署名 + `::warning::` で完走)。

署名対象は**自前 PE の4本のみ**: `FindMyFiles.exe` / `fmf.exe` / `fmf-service.exe` / `fmf_engine.dll`。同梱の
.NET / WindowsAppSDK ランタイム DLL は Microsoft 署名済みのため再署名しない。

## 根拠

- **Azure Artifact Signing(旧 Trusted Signing)は不採用**: マネージドで CI 統合も容易(当初 `release.yml` は本サービスで
  配線していた)だが、2026年現在 **個人枠が米/加/EU/英に限定**され、**日本在住の個人は申し込めない**。地理要件で脱落。
- **EV は不採用(IV を採用)**: EV は2024年3月以降 **SmartScreen の即時信頼を付与しなくなった**(Microsoft 公式)。
  SmartScreen は発行元証明書 + ファイルハッシュの DL 実績で評判が貯まる純評判ベースで、EV/OV/IV で「初回警告 → 実績で解消」は
  同じ。本アプリは**カーネルドライバを積まない**(やらないことリスト)ため、EV の残る実利(ドライバ署名・法人調達要件)は無い。
  個人名義で取得でき最安の **IV** が合理的。予算(年10万円)では EV も射程だが対価が「肩書きのみ」。
- **SSL.com eSigner を採用**: クラウド HSM 署名でハードウェアトークンが runner に不要。TOTP による完全無人の CI 署名。
  GitHub Action(`SSLcom/esigner-codesign`)あり。**個人 IV** と、法人登記不要の **Sole Proprietor EV** の両方に対応し、
  日本から取得可能。「全部お任せのマネージド署名」要件に最も合致。
  - 代替の Certum 個人(約$50/15か月)は最安だが SimplySign が署名毎にスマホ OTP を要求し**無人 CI と相性が悪い**。
    SignPath 財団(FOSS 無償)は要審査で新興プロジェクトは保留され得る。いずれも「放り投げマネージド」要件で劣る。
- **署名を YAML ステップに保持(xtask に入れない)**: 署名は GitHub Secrets と Action に依存する CI 環境固有処理であり、
  `xtask/` が集約する「移植可能なリリース手続きロジック」ではない。Azure 版の前例(YAML ステップ)を踏襲。
- **自前 PE のみ署名**: MS ランタイム DLL の再署名は eSigner クォータの浪費かつ他者著作物への署名で無意味。staging ディレクトリ
  に4本だけ集めて `batch_sign`(1 OTP)し、コピーバック後に `Get-AuthenticodeSignature` で**ハード検証**(署名要求時に
  黙って未署名のまま成功させない = 「黙らない」原則)。

## 影響

- `release.yml` の署名ステップは Azure → SSL.com eSigner に差し替え済み。ゲート `HAVE_SIGNING` は
  `ES_USERNAME` + `CREDENTIAL_ID` の有無で判定。有効化は **Secrets を4つ入れるだけ**(docs/SIGNING.md の D)。
- 公開信頼証明書は最長 ~460日(CA/Browser Forum 2026)で失効。更新手順は docs/SIGNING.md。
- 署名は**タグ駆動 `release.yml` 限定**。`ci.yml`(PR/push)では署名しない(開発中間物は配らない・クォータ温存・フォーク PR は
  Secrets 不可)。

## 再検討トリガ

- Azure Artifact Signing が**日本の個人**に開放されたら、マネージド度・CI 親和性で再評価。
- 本プロジェクトが**カーネルドライバ**を持つことになったら EV が必須要件化。
- **法人 EV 調達要件**(企業配布・ストア要件等)が発生したら Sole Proprietor EV / 法人 EV を再検討。
- SmartScreen の評判モデルが変わり、署名種別で初回挙動に差が再び生じたら見直す。
