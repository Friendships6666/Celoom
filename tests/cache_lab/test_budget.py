"""Offline regression tests: no credential lookup, sockets, or paid calls."""
import os,unittest
from unittest.mock import patch
import paid_ab

class BudgetTests(unittest.TestCase):
    def setUp(self):
        self.environment=patch.dict(os.environ,{},clear=True);self.environment.start()
        paid_ab._LOW_COST_REQUESTS=0
    def tearDown(self):self.environment.stop()
    def test_small_synthetic_request_is_allowed_without_network(self):
        paid_ab.validate_low_cost_budget({'input':[{'role':'user','content':'synthetic'}],'max_output_tokens':256})
    def test_long_input_and_output_fail_before_http_client_creation(self):
        variants=[{'input':[{'role':'user','content':'x'*33000}],'max_output_tokens':256},{'input':[{}]*25,'max_output_tokens':256},{'input':[],'max_output_tokens':81920}]
        for body in variants:
            with self.subTest(body_size=len(str(body))),patch.object(paid_ab.http.client,'HTTPSConnection')as client:
                with self.assertRaisesRegex(RuntimeError,'no HTTP request made'):
                    paid_ab.request(type('URL',(),{'hostname':'127.0.0.1','port':1,'path':'/'})(),'dummy',body,{})
                client.assert_not_called()
    def test_request_count_is_bounded(self):
        paid_ab._LOW_COST_REQUESTS=4
        with self.assertRaisesRegex(RuntimeError,'four requests'):
            paid_ab.validate_low_cost_budget({'input':[],'max_output_tokens':256})
    def test_explicit_override_does_not_change_the_body(self):
        body={'input':[{'role':'user','content':'x'*33000}],'max_output_tokens':81920}
        original=str(body)
        with patch.dict(os.environ,{'JUSTAI_CACHE_LAB_ALLOW_HIGH_COST':'1'}):paid_ab.validate_low_cost_budget(body)
        self.assertEqual(str(body),original)
if __name__=='__main__':unittest.main()
