#!/usr/bin/env python3
"""Offline, exact procurement for verified fixed-price, fixed-fee supplier quotes.

Does not scrape, upload a build, authenticate, mutate carts or place orders.
See research/supplier-apis/README.md for limitations and input schema.
"""
import argparse
from decimal import Decimal
import itertools
import json


def money(value):
    result = Decimal(str(value))
    if not result.is_finite() or result < 0:
        raise ValueError("Prices and fixed fees must be finite and nonnegative")
    return result


def count(value):
    if type(value) is not int or value < 0:
        raise ValueError("Quantities must be nonnegative integers")
    return value


def optimize(data):
    """Maximize covered units, then minimize cost, over every supplier subset.

    Exact only for the stated independent-stock, linear-price, fixed-fee model.
    Currency conversion and part/colour/condition matching must happen upstream.
    """
    currency = data["currency"]
    lots = {}
    for row in data["lots"]:
        key = (str(row["part"]), int(row["ldraw_color"]))
        if key in lots:
            raise ValueError("Duplicate requested part/colour lot")
        lots[key] = count(row["quantity"])
    suppliers = sorted(data["suppliers"], key=lambda s: s["id"])
    if len(suppliers) > 12:
        raise ValueError("At most 12 suppliers; exact subset search is exponential")
    if len({s["id"] for s in suppliers}) != len(suppliers):
        raise ValueError("Supplier IDs must be unique")
    offers, fees = {}, {}
    for s in suppliers:
        if s["currency"] != currency:
            raise ValueError("Normalize currencies explicitly before optimization")
        if s.get("pricing_model") != "linear_fixed_fee":
            raise ValueError("Tiered prices, thresholds and minimum orders require another model")
        fees[s["id"]] = money(s["fixed_fee"])
        seen = set()
        for o in s["offers"]:
            key = (str(o["part"]), int(o["ldraw_color"]))
            if key in seen:
                raise ValueError("Duplicate supplier offer: verify mappings before combining")
            seen.add(key)
            if o.get("verified") is not True:
                raise ValueError("Only verified part/colour/brand/condition/stock quotes are accepted")
            offers.setdefault(key, []).append((money(o["unit_price"]), s["id"], count(o["max_quantity"])))
    best = None
    for size in range(len(suppliers) + 1):
        for subset in itertools.combinations([s["id"] for s in suppliers], size):
            enabled = set(subset)
            allocations, missing, used, items = [], [], set(), Decimal(0)
            covered = 0
            for (part, color), quantity in sorted(lots.items()):
                left = quantity
                for price, supplier, capacity in sorted(offers.get((part, color), [])):
                    if supplier not in enabled:
                        continue
                    take = min(left, capacity)
                    if take:
                        cost = price * take
                        allocations.append(dict(part=part, ldraw_color=color, supplier=supplier,
                                                quantity=take, unit_price=str(price), subtotal=str(cost)))
                        used.add(supplier)
                        covered += take
                        items += cost
                        left -= take
                    if not left:
                        break
                if left:
                    missing.append(dict(part=part, ldraw_color=color, quantity=left))
            fixed = sum((fees[s] for s in used), Decimal(0))
            score = (-covered, items + fixed, len(used), tuple(sorted(used)))
            if best is None or score < best[0]:
                best = (score, dict(currency=currency, covered_units=covered,
                                   requested_units=sum(lots.values()), complete=not missing,
                                   item_subtotal=str(items), fixed_fees=str(fixed),
                                   modeled_total=str(items + fixed), allocations=allocations,
                                   missing=missing, suppliers=sorted(used)))
    result = best[1]
    result["scope"] = "Exact for supplied verified linear prices, independent stock and fixed fees only; not a live checkout quote."
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("quotes", help="Normalized verified quote JSON; no network requests")
    args = parser.parse_args()
    with open(args.quotes) as f:
        print(json.dumps(optimize(json.load(f)), indent=2))
