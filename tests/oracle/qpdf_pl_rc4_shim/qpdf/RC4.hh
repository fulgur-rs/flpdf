#ifndef FLPDF_QPDF_PL_RC4_PROBE_RC4_HH
#define FLPDF_QPDF_PL_RC4_PROBE_RC4_HH

#include <qpdf/RC4_native.hh>

class RC4
{
  public:
    RC4(unsigned char const* key_data, int key_len = -1) :
        impl(key_data, key_len)
    {
    }

    void process(unsigned char const* in_data, size_t len, unsigned char* out_data)
    {
        impl.process(in_data, len, out_data);
    }

  private:
    RC4_native impl;
};

#endif // FLPDF_QPDF_PL_RC4_PROBE_RC4_HH
