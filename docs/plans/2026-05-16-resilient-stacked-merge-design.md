# 耐障害スタック PR マージ + CodeRabbit 中断回復ワークフロー — 設計

- 日付: 2026-05-16
- beads: flpdf-418
- ステータス: 設計のみ（実装はしない）
- 元になった2要望: (1) PR マージ時のワークフロー改善、(2) rate limit で止まった時の回復改善 — 統合済み

## 1. 目的とスコープ

スタック PR（`sub-1..N`）を **bottom から1本ずつ** main へ落としていく
自律ワークフローを定義する。CodeRabbit の各種中断（pause /
rate-limit / CHANGES_REQUESTED）から自動回復し、rate-limit /
コンテキストコンパクション / セッション死で途切れても次セッションが
安全に再開できることを保証する。

現フェーズでは**人間レビューは原則不要**。安全網は CI が回す
qpdf byte-identical 検証 + roborev + CodeRabbit トリアージ + 任意の
qtest であり、人間は最終手段のエスケープにのみ登場する。

## 2. 確定した決定事項

| 項目 | 決定 | 根拠 |
|---|---|---|
| マージ単位 | bottom から1本ずつ | ユーザ選択 |
| スタック root | main（現状: #122 sub-1 → base:main、sub-2..6 連鎖） | 実機確認 |
| main 保護 | PR必須 + CI(quality,test)必須 + force-push禁止 + ブランチ削除禁止。up-to-date 不要。GitHub の必須承認は設定しない | ユーザ選択。up-to-date 必須はスタックと相性が悪い |

> **保護対象は main のみ**。`epic/flpdf-9hc-5-8/sub-*` のスタック
> ブランチは保護しない（§7 cascade の `gh stack sync` が
> `--force-with-lease` で stack ブランチへ force-push するため、
> ここを保護すると cascade が破綻する）。「force-push 禁止」は
> main にのみ適用。
| CodeRabbit 承認 | **self-imposed 台帳ゲート**。AI が review state を読んで判定。CodeRabbit 側のレビュー/approve 設定は構築済み | `coderabbitai[bot]` の repo 権限が `none` のため GitHub の必須承認には数えられない（実機確認）。CodeRabbit 設定済みも確認（#124=APPROVED, #126=CHANGES_REQUESTED） |
| 人間レビュー | 現フェーズでは原則不要 | byte-identical harness + qtest + roborev + CodeRabbit が安全網 |
| 耐障害方式 | 専用台帳を**持たない**。冪等フェーズ + 観測からのステートレス再構築 | 台帳は第2の真実源になり drift する。再開状態は全て権威ある観測源から再導出可能 |

### 2.1 CI が byte-identical 安全網そのものである（重要）

`.github/workflows/ci.yml` の `test` ジョブは Linux で qpdf を
インストールし、以下を実行する:

- `cargo test -p flpdf-cli --test compat_matrix_tests`
- `cargo test -p flpdf --features qpdf-zlib-compat --test zlib_compat_tests`（bytes-identical zlib compat）
- `cargo test -p flpdf-cli --test compat_baseline_static_id -- --nocapture`
- `cargo test -p flpdf-cli --test compat_matrix_baseline -- --nocapture`（drift が明示ステップで露見）

したがって「CI required」は薄いゲートではなく、**qpdf byte-identical
検証そのもの**。これが人間レビュー縮小の正当化根拠。

> 設定要件: branch protection の required status checks に、この
> compat matrix を回す CI ジョブ（`test`）を必ず含めること。これが
> 抜けると安全網が外れる。

qtest（`flpdf-qtest` の別 workspace）は CI 外。ループが任意で追加
実行できる追加検証であり、in-band の byte-identical ゲートは CI の
compat matrix 側が担う。

## 3. 全体フロー

```
[derive_state] → [Gate] → [Merge] → [Cascade] → 次の bottom へ
```

各フェーズは冪等。どこで中断しても、次セッションは `derive_state()`
で観測から再開点を一意に復元して続行する。

### 3.1 derive_state()（台帳の代替）

セッション開始時に gh/git 問い合わせ数発で状態を再計算する。

| 知りたい状態 | 権威ある源 |
|---|---|
| マージ済み集合 | 各スタック PR の state==MERGED |
| 現 bottom (cursor) | OPEN な最小 `sub-K` |
| cascade 残あり? | `git merge-base --is-ancestor origin/main <upstack>` が偽 |
| cleanup 残あり? | `delete_branch_on_merge=true` でサーバ側自動。ローカルは `gh stack sync` |
| CodeRabbit 中断と resume_at | 対象 PR の CodeRabbit コメント本文を `gh pr view --comments` で再読 |

台帳は持たない。実環境（`gh pr view`, `git log origin/main`,
CodeRabbit コメント）が常に唯一の真実源。
（subtask の `bd close` は従来のタスク管理として別途残す。耐障害
ステートとタスク管理を混ぜない。）

## 4. Pre-merge ゲート（self-imposed, non-zero で落ちる）

bottom PR が次を**全て**満たして初めて Merge へ。1つでも偽なら停止
理由を出力して exit（自己申告でなく機械判定）。

1. **CI green**: `gh pr checks <bottom> --required` が全 success（quality, test）。`test` が前述の byte-identical 検証を内包
2. **CodeRabbit ゲート（次のいずれか）かつ SHA 一致**:
   - `coderabbitai` の最新 review が `APPROVED`、**または**
   - §6 Resolve サブループ完了 = その PR の actionable finding 集合の**全件**が「fix 済み」または「followup issue 起票 + スレッド返信済み」で disposition されている

   **かつ** その判定の根拠（APPROVED review commit、または最後に
   fix を push した commit）== 現 PR head SHA。SHA 一致が肝 —
   cascade で force-push された後の古い承認/古い disposition を
   無効化する。
   （= §6.1 のゲート意味論と同一。実装者は §4 と §6 を必ず併読）
3. **roborev**: 該当 review が pass。**roborev は GitHub check では
   なくローカル CLI/daemon**（`roborev`）。`.roborev.toml` の
   `review.failed` / `review.completed` hook で結果を beads issue に
   反映する。**force-push では自動再評価されない**ため、cascade /
   fix push で head SHA が変わったら旧結果は無効化扱いとし、§5/§7 の
   状態機械側で `roborev review <HEAD>`（または `roborev-review-branch`
   skill）を**明示的に再キック**して最新 SHA に対する `completed`
   pass を取り直す（古い roborev 結果での gate 通過を防ぐ）
4. **Compat matrix**: PR 本文テンプレの2チェックボックスの tick は
   AI が判定する。`tests/golden/compat-matrix.md` /
   `tests/golden/baseline-static-id.md` に
   **drift 無し** → AI が自動で tick（テンプレ規定どおり box は無
   作業 tick 可）。**drift 有り** → 自動 tick せず**人間フラグ**
   （意図的 re-bless か否かの判断、`docs/qpdf-compat-decisions.md`
   追記要否を含む）

## 5. CodeRabbit 状態機械（中断の自動回復）

cascade は1本マージごとに上位 PR を force-push する＝「頻繁な修正」に
該当しやすい。よって pause / rate-limit は稀な事故ではなく**常発
イベント**であり、クリティカルパスとして設計する。

`derive_state()` は bottom PR の CodeRabbit 状態を次に分類する:

| 分類 | 観測シグナル | 回復（冪等） | 担当 |
|---|---|---|---|
| **APPROVED** | 最新 review = `APPROVED` かつ review commit == head SHA | ゲート通過 → Merge | 自動 |
| **paused** | コメントに `Resume review` 未チェックボックス | その特定行のみ `- [ ]`→`- [x]` に編集して resume → 再評価 | 自動 |
| **rate-limited** | コメントが rate-limit 文言 + 再試行時刻 | リセット時刻まで在席待機 → `@coderabbitai review` 再投稿 → 再評価 | 自動 |
| **in-progress / 未レビュー** | review イベント未着 | 短間隔ポーリング | 自動 |
| **CHANGES_REQUESTED** | 最新 review = `CHANGES_REQUESTED` | §6 Resolve サブループ | 自動 |

優先順位: paused と rate-limited が同時なら **rate-limited を先に
解消**（待機が解けないと resume しても再び止まる）。

### 5.1 paused 回復の実装上の注意（設計判断点）

- CodeRabbit コメントには複数チェックボックス（summary トグル等）が
  あり得る。**ラベル文字列 `Resume review` で行を特定**し、その行
  のみフリップ。誤って別ボックスを立てない
- 機構: 対象コメント ID 特定 → `gh api .../issues/comments/{id}` で
  body 取得 → 該当行のみ `[ ]`→`[x]` → PATCH → 再取得して反映確認
- 冪等: 既に `[x]`（または既に APPROVED/SHA一致）なら何もしない

### 5.2 rate-limit 待機の実装機構（**未決定**）

この環境は長時間の前景 `sleep` を禁止（Bash tool 仕様）。
「ScheduleWakeup/loop は不要、sleep で待機」というユーザ希望と機構が
衝突する。設計としては「リセット時刻まで在席待機 → 再コメント」で
固定するが、実装機構は実装時に次から選ぶ:

- (a) `sleep` を `run_in_background` で投げ、完了通知で再開
- (b) Monitor の until 条件で待機

→ 設計書では**未決定**として残し、実装フェーズで決定する。

## 6. CHANGES_REQUESTED → Resolve サブループ

既存 `roborev-refine` / `resolving-ai-review` skill の機構を再利用し、
「トリアージ→followup 起票」分岐を足す。

### 6.1 ゲート意味論の変更（採用、ただし規律つき）

> 旧: 「CodeRabbit APPROVED」必須
> 新: 「全 actionable finding が **fix 済み or followup issue 起票+
> スレッド返信済み**」必須。main は GitHub 承認強制なし
> （self-imposed）なのでこのゲート定義は我々が握れる。CodeRabbit が
> 名目上 CHANGES_REQUESTED のままでもマージ可。

### 6.2 手順

1. CodeRabbit の actionable finding 群を取得
2. **トリアージ（保守バイアス）**:
   - 明確かつ安全に直せる → fix-now
   - 判断/設計を要する・スコープ外 → followup: `bd create` で issue
     起票し、**その finding のコメントスレッドへ直接返信**して issue
     参照を残す（`comment-id-replies` 運用に従う）
   - 安全に defer してよいか不明 → defer せず fix-now か人間フラグ
     （迷ったら自動 defer しない）
3. fix-now をまとめて commit → PR ブランチへ push（head SHA 変化
   = 旧 APPROVED 無効化は意図通り。「頻繁な修正」で pause を誘発
   しうるが §5 が処理）
4. `@coderabbitai review` 再投稿 → §5 状態機械へ再突入
5. 再評価: 全 finding が fix済み or followup起票済み かつ CI green
   かつ SHA 整合 → ゲート通過
6. **反復上限 N**（roborev-refine 同様）。収束しなければ停止して
   人間へ（無限 fix↔review 防止）

**finding は1つも黙って落とさない** — 必ず fix か tracked。これが
ゲートの健全性条件。

## 7. Merge と Cascade

1. **Merge**: ゲート全 true の bottom に `gh stack merge <bottom>`
   （cumulative だが bottom 指定なので1本のみ）。
   `delete_branch_on_merge=true` がリモートブランチを削除
2. **Cascade**: `gh stack sync`（fetch → trunk ff → カスケード
   rebase → atomic force-push → GitHub PR base 同期）。コンフリクト
   時は全ブランチ原状復帰 + `gh stack rebase` 案内 → **人間フラグ**
   （自動解決しない）
3. cascade が上位 PR を force-push → CodeRabbit 再レビュー + roborev
   再キックが走る → §5 状態機械 + §4-3 が処理（pause/rate-limit は
   ここで頻発する前提）

> **PR auto-retarget の経路に注意**: bottom (sub-1) がマージされ
> `delete_branch_on_merge=true` でブランチ削除されると、GitHub は
> sub-2 PR の base を main へ auto-retarget する。これは force-push
> とは**別経路**で、この瞬間 sub-2 の diff が見かけ上膨張する。
> auto-retarget を CodeRabbit が「巨大な新規変更」として全再レビュー
> するか何もしないかは挙動依存（§11 で実機確認）。`gh stack sync`
> が GitHub PR base を同期するので最終的に base は正しくなるが、
> retarget 直後の CodeRabbit 状態は §5 状態機械で必ず分類し直す。

ロールバック: Merge は API 完了で原子的、`gh stack sync` は atomic
（`--force-with-lease --atomic`）。中途半端な部分マージは構造的に
発生しない。明示 rollback は不要 — **冪等再実行が正す**のが安全策。

## 8. 人間判断フラグ（現フェーズでは最小）

- 自動: pause / rate-limited / in-progress / CHANGES_REQUESTED
  （Resolve サブループ）
- 停止して人間へ:
  - Resolve が反復上限で非収束
  - CI failure が修正で直らない
  - `gh stack sync` コンフリクト原状復帰
  - Compat matrix baseline drift で要判断
  - トリアージで「defer 可否不明」が出た finding

## 9. 代替モード: epic 受け入れブランチ（フォールバック）

直接 main 運用に支障がある場合の構造的代替。

- main 起点で `epic/flpdf-9hc-5-8` 受け入れブランチを作成
- スタックを rebase し sub-1 の base を main → 受け入れブランチへ
- 全スタック PR を受け入れブランチへマージ（§3〜§7 と同じ機構、
  ただしマージ先 = 受け入れブランチ。main を触らないため低リスク）
- 最後に「受け入れブランチ → main」PR で、必要なら人間レビュー +
  フル CI + qtest を1回だけ実施

**モード切替基準**（どちらかが真なら代替モードを検討）:

- 最後にまとめて1回の人間 / qtest パスを置きたい
- main の branch protection が高速スタックマージと干渉する
- epic を main 履歴上で原子的にしたい

デフォルトは §3〜§8 の **直接 main 自律モード**（現スタックは既に
main 起点なので追加構造不要）。

## 10. 残存リスクと安全網

main 保護は PR + CI のみで、CHANGES_REQUESTED 自動解決により人間
レビューの網は薄い。これは現フェーズの意図的判断であり、安全網は
次の5点に依存する:

1. CI の qpdf byte-identical 検証が required status check に含まれる
   こと（§2.1 設定要件）
2. finding を1つも黙って落とさない規律（§6）
3. defer 可否不明は人間フラグ（§6.2）
4. 修正後も CI green 必須（§4-1）
5. Resolve 反復上限 + 人間エスケープ（§6.2-6, §8）
6. 任意で qtest を追加実行できる（§2.1）

## 11. 未決定事項と推奨デフォルト

skill 化の RED/GREEN 検証（flpdf-1oe）で次が解決済み（実装時に最終確認）:

- **解決**: roborev はローカル CLI/daemon、force-push で自動再評価
  されない → 再キックは明示 `roborev review <HEAD>`（§4-3 に反映）
- **解決**: Compat-matrix golden は `tests/golden/` 配下（§4-4 に反映）
- **推奨デフォルト**: §5.2 rate-limit 待機 = **Monitor の until 条件
  でリセット時刻まで待機**（前景 long sleep 禁止の制約下で最も確実。
  fallback: `run_in_background` sleep）。実装時に確定
- **推奨デフォルト**: §6.2 Resolve サブループ反復上限 **N = 3**
  （`roborev-refine` 慣例に一致）。実装時に確定

実機確認が残るもの:

- §7 bottom マージ後の **PR auto-retarget に対する CodeRabbit の
  挙動**（全再レビュー or 無反応 or rate-limit 誘発）

成果物の形:

- **決定済み**: 実行手順は project skill
  `.claude/skills/stacked-merge/SKILL.md`（RED/GREEN 検証済み、
  flpdf-1oe）。本設計書は why、skill は how。bd remember に短い
  ポインタを登録
