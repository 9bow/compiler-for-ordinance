# compiler-for-ordinance

> **현재 상태**: 플레이스홀더. 실제 컴파일 로직은 Phase 4 이후 구현됩니다.

## 소개

`compiler-for-ordinance`는 `.cache/ordinance/*.xml` 파일을 베어(bare) Git 저장소로 변환하는 Rust 도구입니다. 자치법규(조례·규칙) 데이터를 [`legalize-kr/legalize-kr`](https://github.com/legalize-kr/legalize-kr) 방식과 동일하게 Git 이력 DB로 구성하는 것을 목표로 합니다.

법령용 컴파일러인 [`legalize-kr/compiler`](https://github.com/legalize-kr/compiler)와 동일한 구조를 따릅니다.

## 저장소

- **정식 저장소**: [`legalize-kr/compiler-for-ordinance`](https://github.com/legalize-kr/compiler-for-ordinance)
- **초기 호스팅**: [`9bow/compiler-for-ordinance`](https://github.com/9bow/compiler-for-ordinance)

## 사용법

```bash
cargo build --release
./target/release/compiler-for-ordinance --cache-dir .cache/ordinance --output-dir output.git
```

현재는 메시지만 출력하고 종료합니다 (플레이스홀더).

## 관련 저장소

| 저장소 | 설명 |
|---|---|
| [`legalize-kr/compiler`](https://github.com/legalize-kr/compiler) | 법령용 컴파일러 (형제 프로젝트) |
| [`legalize-kr/legalize-kr`](https://github.com/legalize-kr/legalize-kr) | 법령 데이터 저장소 |
| [`legalize-kr/legalize-pipeline`](https://github.com/legalize-kr/legalize-pipeline) | 수집·변환 파이프라인 |
