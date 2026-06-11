# ADR-0015: WinUI 3データ仮想化(非ジェネリックIList+INCC+IItemsRangeInfo)

日付: 2026-06-11 / 状態: 採用済み

## 決定

結果リストの仮想化は非ジェネリック `IList` + `INotifyCollectionChanged` + `IItemsRangeInfo` + プレースホルダで行う(VirtualResultList)。`ISupportIncrementalLoading`、ItemsView / ItemsRepeater は使わない。ItemsPanelはItemsStackPanel固定。VirtualResultListはページと同寿命の単一インスタンス(x:Bind OneTime)とし、ItemsSourceは差し替えない。新結果は `Reassign`(プリフェッチ済みseed適用+INCC Reset 1回)で公開し、エンジンが `QueryTrace.unchanged=true`(同一クエリ・全ボリュームでID列がmemcmp一致)を返した再クエリは `RefreshInPlace`(Resetなし・可視行のin-place充填・件数テキスト不変)とする。

## 根拠

- 件数既知ランダムアクセスの仮想化は「非ジェネリックIList+INCC+IItemsRangeInfo+プレースホルダ」が現行WASDKのサポート明記。`IList<T>` のみでは動かない(microsoft-ui-xaml#1809)
- `ISupportIncrementalLoading` はクラッシュ報告があり回避(microsoft-ui-xaml#6883)
- ItemsView / ItemsRepeater は上記インターフェース非対応。ItemsPanelをItemsStackPanel以外にすると仮想化が無効化される
- ItemsSource差し替えはListViewの仮想化状態を破棄し、ちらつきが再発する
- Windowsはアイドルでも無音にならない(ログ・テレメトリ等のUSNバッチ)。IndexChanged起因の再クエリが200ms毎に同一結果を返すため、Reset再発行では画面が恒常的にチャーンする — unchanged時のRefreshInPlace(MVVMセッターは値変化時のみ通知)で同一画面の再描画をゼロにする

## 影響

- IList在籍契約: 在籍 = 「indexがCount未満、かつ現在のページキャッシュの該当スロットがその同一インスタンス」。偽の「在籍」は `GetAt(staleIndex)` でXAML深部のクラッシュになる(実証: 結果ありの検索→全消去で `Int32.MaxValue-1` 例外が確実に再現。修正A/B: UIAストレスで旧コード4エラー→0)
- indexerは範囲外即throw・絶対にフェッチしない(プレースホルダ返却)。列挙/CopyToはページLRU(上限4096行)を乱さない
- Reassign/RefreshInPlaceのUIスレッド検査はRelease常時有効
- in-place更新では値が変わったセル(伸びたファイルのsize等)だけ更新される

## 再検討トリガ

- WASDKがItemsView系に件数既知ランダムアクセス仮想化(IItemsRangeInfo相当)を正式提供した場合
