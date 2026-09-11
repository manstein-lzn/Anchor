"""The two conditions, and the answers they are scored against.

This is a cognition-handoff experiment. A point is chosen in a real run, before its
first writing attempt, and a fresh agent is asked the same seven questions about the
task. In one condition it is given everything the run had accumulated — the raw
evidence ledger, verbatim. In the other it is given only a cognition state: the same
task rendered as contract, situation, experience, intent and an index of where detail
lives, at roughly one percent of the size.

The seven questions are the quality criterion the archived project defined: can a
fresh agent, seeing only what it is given, continue without guessing a critical
decision or reconstructing the task from full history?

Condition B is authored to be *sufficient*, not to be small for its own sake. That is
the point: if a state designed against this criterion passes and the raw material does
not, the criterion is measuring something. If both pass, the state is unnecessary. If
neither passes, the criterion is wrong.

Everything in the expected answers below is derived from the run's own persisted
data, not from recollection. The counts were recomputed before this file was written.
"""

from __future__ import annotations

#: Asked verbatim in both conditions. Chinese, because the run's request language is
#: Chinese and the paper is to be written in it.
QUESTIONS = """这是一个进行中的任务。请只依据下面给你的内容回答七个问题。

回答要具体。**如果给你的内容不足以回答某一问，直接说"无法从给定内容确定"，
不要猜测，也不要泛泛而谈一个学术综述通常怎么做。**

1. 长期目标是什么？什么明确在范围之外？
2. 当前什么是已经确立的？什么是尚不确定或相互冲突的？什么是受阻的？
3. 用户当前想要什么？
4. 哪些决策与约束仍然主导着选择？
5. 哪些失败路径不可重复？在什么条件下才允许重试？
6. 当前的缺口是什么？下一个具体动作是什么？
7. 昂贵而已被省略的细节，可以在哪里被精确恢复？
"""

#: Each expected answer is a list of facts. A fact counts only if it is stated; the
#: grader is asked whether each appears, which is a question about the text rather
#: than about taste. Derived from the run's persisted snapshot and node runs.
EXPECTED: dict[int, dict[str, object]] = {
    1: {"question": "长期目标与范围之外",
        "facts": [
            "产出一篇**面向读者的中文学术综述**（不是检索报告、不是审计台账）",
            "主题是**编译器优化中的代价模型（cost model）近年的发展**",
            "读者是懂机器学习/系统、但不在编译器优化细分领域的人",
            "范围之外至少提到一项：非编译器领域的代价模型（数据库/网络/纯硬件功耗）",
            "范围之外至少再提到一项：无定量实验的观点/演示类文献，或非英文文献",
        ]},
    2: {"question": "已确立 / 不确定 / 受阻",
        "facts": [
            "已确立：**59 篇来源**通过验证",
            "已确立：其中**24 篇读过全文**（其余为摘要级或题录级）",
            "已确立：演化在**三个层次**上都成立（指令级 / 张量·循环程序 / 编译遍决策）",
            "冲突的核心是**学习型 vs 解析式**代价模型",
            "该冲突的具体形态：学习型精度占优但依赖逐微架构大规模真机测量与长时训练、跨平台迁移差",
            "受阻或薄弱：**编译遍决策层次证据最薄**，或有谱系尚未完成前向追踪",
        ]},
    3: {"question": "用户当前想要什么",
        "facts": [
            "**现在开始写作**（不是继续检索、不是再做一轮规划）",
        ]},
    4: {"question": "仍然主导选择的决策与约束",
        "facts": [
            "来源数下限 **24 篇**、全文阅读下限 **14 篇**（均已满足）",
            "只用数字引用编号，**不得包含参考文献章节**（由系统生成）",
            "正文**不得出现过程语言**（如「本轮检索」「本次共执行」这类句子）",
            "正文**不得出现证据级别标签**（如全文级/摘要级/题录级）",
            "段落长度受限（约 1200 字符）",
            "结果型数字必须有**读过全文**的来源支撑",
            "不得复述摘要，必须基于论文正文",
        ]},
    5: {"question": "不可重复的失败路径与重试条件",
        "facts": [
            "有一轮检索因**返回结果全部无关**而被整轮否决（宽泛关键词在全文索引上噪声极高）",
            "该失败的重试条件：**改用机制性关键词**（工具名/数据集名，如 Ithemal / BHive / AutoTVM / MLGO）",
            "仅有摘要级证据的来源**不得用于支撑结果型数字**，需要先取得全文",
            "完整检索与纳排记录在**检索日志**中（74 条）",
        ]},
    6: {"question": "当前缺口与下一个动作",
        "facts": [
            "缺口：**账本已饱和**（13 轮，边际收益递减），但**正文尚未写出**",
            "下一个动作：**依据账本写出综述正文**",
            "**不是**先补检索 / 先扩大来源（账本已足够继续）",
        ]},
    7: {"question": "细节的精确恢复位置",
        "facts": [
            "每篇来源有**内容寻址的引用**（可精确取回）",
            "59 篇的**结构化笔记**按引用编号对应（问题/方法/数字/结论/局限）",
            "**读过全文的来源**另有全文读取位置，可精确取回全文而非摘要",
            "24 组**对立证据**带证据编号与解决状态",
            "123 条**未解决清单**",
        ]},
}
