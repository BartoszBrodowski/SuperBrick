import unittest
from optimize_supplier_quotes import optimize


def vendor(name, price, fee=0, stock=10):
    return dict(id=name, currency="USD", pricing_model="linear_fixed_fee", fixed_fee=str(fee),
                offers=[dict(part="3005", ldraw_color=4, unit_price=str(price),
                             max_quantity=stock, verified=True)])


class OptimizerTests(unittest.TestCase):
    def run_case(self, vendors, quantity=10):
        return optimize(dict(currency="USD", lots=[dict(part="3005", ldraw_color=4,
                                                        quantity=quantity)], suppliers=vendors))

    def test_shipping_beats_cheaper_sticker_price(self):
        r = self.run_case([vendor("cheap", ".01", 5), vendor("delivered", ".10", 1)])
        self.assertEqual(r["suppliers"], ["delivered"])
        self.assertEqual(r["modeled_total"], "2.00")

    def test_capacity_split_and_missing_are_explicit(self):
        r = self.run_case([vendor("a", ".01", stock=3), vendor("b", ".02", stock=4)])
        self.assertFalse(r["complete"])
        self.assertEqual(r["covered_units"], 7)
        self.assertEqual(r["missing"][0]["quantity"], 3)

    def test_rejects_currency_and_unknown_stock(self):
        v = vendor("a", 1)
        v["currency"] = "EUR"
        with self.assertRaises(ValueError):
            self.run_case([v])
        v = vendor("a", 1)
        v["offers"][0]["verified"] = False
        with self.assertRaises(ValueError):
            self.run_case([v])

    def test_rejects_dynamic_shipping_and_tiers(self):
        v = vendor("a", 1)
        v["pricing_model"] = "free_shipping_threshold"
        with self.assertRaises(ValueError):
            self.run_case([v])

    def test_deterministic_tie_and_fractional_cent_precision(self):
        r = self.run_case([vendor("b", ".003"), vendor("a", ".003")])
        self.assertEqual(r["suppliers"], ["a"])
        self.assertEqual(r["modeled_total"], "0.030")


if __name__ == "__main__":
    unittest.main()
