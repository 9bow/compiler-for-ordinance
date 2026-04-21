# ordinance-kr

대한민국 자치법규 (조례·규칙·훈령·예규) 를 Git 저장소로 관리합니다. 각 자치법규는
Markdown 파일이고, 각 자치법규의 공포일자가 Git commit date로 기록됩니다.

[legalize-kr](https://github.com/legalize-kr/legalize-kr) (대한민국 법령 Git 저장소)의
자매 프로젝트입니다.

> **공지**: 자치법규 수집/변환 파이프라인이 개선될 경우, 전체 자치법규 히스토리를
> 재구성하기 위해 force-push가 실행될 수 있습니다. 이 경우 모든 commit hash가
> 변경됩니다. 이 저장소를 fork하거나 참조하는 경우, force-push 이후
> `git fetch --all && git reset --hard origin/main`으로 동기화해 주세요.

## 구조

```
ordinances/{광역}/{기초|_본청}/{ordinance_type}/{NFC(자치법규명)}/본문.md
```

- `광역`: 17개 광역시·도 (예: 서울특별시, 경기도, 제주특별자치도)
- `기초`: 시·군·구 (없으면 `_본청`)
- `ordinance_type`: 조례 / 규칙 / 훈령 / 예규
