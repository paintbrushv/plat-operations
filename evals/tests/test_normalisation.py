from decimal import Decimal

import pytest

import score


@pytest.mark.parametrize(
    "text, expected",
    [
        ("13,700.00", Decimal("13700.00")),
        ("13700", Decimal("13700")),
        ("$13,700", Decimal("13700")),
        ("13700.00", Decimal("13700.00")),
        ("(13,700.00)", Decimal("-13700.00")),
        ("($5,100)", Decimal("-5100")),
        ("-5,100.00", Decimal("-5100.00")),
        ("−5,100.00", Decimal("-5100.00")),  # unicode minus
        ("-$5,100", Decimal("-5100")),
        ("$-5,100", Decimal("-5100")),
        ("4.17%", Decimal("4.17")),
        ("87.5 percent", Decimal("87.5")),
        ("1,000,000", Decimal("1000000")),
        ("13.7k", Decimal("13700")),
        ("1.5 million", Decimal("1500000")),
    ],
)
def test_normalize_number(text, expected):
    assert score.normalize_number(text) == expected


@pytest.mark.parametrize("text", ["abc", "1.0.0", "", "12 apples 13", "2026-05"])
def test_normalize_number_rejects_non_numbers(text):
    assert score.normalize_number(text) is None


def test_amounts_compare_equal_by_decimal_value():
    envelope_string = Decimal("13700.00")
    for written in ("13,700.00", "13700", "$13,700"):
        assert score.normalize_number(written) == envelope_string


def test_extract_numbers_ignores_identifiers_dates_and_list_numbers():
    text = (
        "1. NOI was $13,700.00 for 2026-05 (review_period@1.0.0, trace "
        "702e281e-c352-41a5-8fad-3774c5b782fc, output sha256:b82de9474840…).\n"
        "2. Budget was 18,800."
    )
    found = [(n.raw, n.value) for n in score.extract_numbers(text)]
    assert found == [("$13,700.00", Decimal("13700.00")), ("18,800", Decimal("18800"))]


def test_extract_numbers_flags_percent_and_places():
    (number,) = score.extract_numbers("a shortfall of 27.13%")
    assert number.pct and number.places == 2 and number.value == Decimal("27.13")


def test_words_with_digits_are_not_numbers():
    assert score.extract_numbers("T12 and Q3 and 10th and H1") == []


def test_parenthesised_count_is_not_a_negative():
    (number,) = score.extract_numbers("the property (24 units) is small")
    assert number.value == Decimal("24")


def test_get_path_walks_dicts_and_lists():
    obj = {"result": {"revisions": [{"unit_count": 24}]}}
    assert score.get_path(obj, "result.revisions.0.unit_count") == 24
    assert score.get_path(obj, "result.revisions.1.unit_count") is score.MISSING
    assert score.get_path(obj, "result.nope") is score.MISSING


def test_segments_do_not_split_on_decimal_points():
    text = "NOI was 13,700.00 and budget 18,800.00. Revenue fell.\n- bullet 1.5 here"
    pieces = [text[a:b] for a, b in score.segments(text)]
    assert pieces == ["NOI was 13,700.00 and budget 18,800.00.", "Revenue fell.", "- bullet 1.5 here"]
